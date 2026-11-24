//! Which tables sync between a reader's devices, and how, declared for the fongo-sync kernel.
//!
//! The kernel captures writes by trigger and merges changes field by field. A table not named
//! here stays on this device.
//!
//! Left out for now:
//! - `settings`: some keys belong to one machine, and the kernel cannot sync only some rows.
//! - `device_books`: its key column is not declared NOT NULL, which a natural key needs.
//! - `annotation_alternatives`: not yet decided whether set-aside annotations travel.

use std::path::Path;

use fongo_sync_client::{Apply, Error as ClientError, merge_applied};
use fongo_sync_sqlite::core::{Change, Strategy};
use fongo_sync_sqlite::{Applied, Collection, LocalDefault, Registry, Replica};
use sqlx::SqliteConnection;

use crate::error::{EngineError, Result};
use crate::storage::{Storage, clashes, synced};

#[expect(clippy::too_many_lines)]
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
            .identity(&["name"]),
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
            .local_dependent("note_links", "from_note")
            .local_dependent("note_links", "to_note"),
        Collection::natural("note_bodies", "note_bodies", &["note_id"])
            .owner("note_id", "notes")
            .field_reporting_conflicts("body"),
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
        Collection::natural("review_ratings", "review_ratings", &["note_id"])
            .owner("note_id", "notes")
            .reference("scale_id", "rating_scales")
            .field("value"),
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

/// How many times one settle gives up rows that keep held rows out and tries those again. What
/// still waits after that is reported held and tried by the next settle.
const SETTLE_ROUNDS: usize = 4;

/// Applies what a sync receives and, once a pull has read the whole log, settles clashes: the
/// engine's side of the kernel client's [`Apply`].
///
/// Everything it does happens in the database, in the transaction the client hands it. Nothing
/// is left to do after the commit: covers to fetch and cover files no book uses are read from
/// the database once a sync succeeds ([`after_sync`]), so a failed, cancelled or interrupted
/// sync loses nothing. The receiver keeps nothing between calls, so an apply the client repeats
/// after a rollback starts from what the database holds.
#[derive(Default)]
pub(crate) struct Receiver {
    /// The vault, to check received notes' file paths against; without one, only their text is.
    pub(crate) vault: Option<std::path::PathBuf>,
    #[cfg(test)]
    pub(crate) faults: Faults,
}

/// Failures a test makes the receiver suffer.
#[cfg(test)]
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each fault is switched on alone"
)]
pub(crate) struct Faults {
    /// Fail this many applies that were given changes, after doing their work.
    pub(crate) fail_applies: usize,
    /// End the client's transaction inside a savepoint of the receiver's own, once.
    pub(crate) end_transaction: bool,
    /// As `end_transaction`, then take the write lock in a transaction nothing will end, once.
    pub(crate) hold_write_lock: bool,
    /// Never return from an apply that was given changes, once its writes are made.
    pub(crate) hang: bool,
    /// Panic in an apply that was given changes, once its writes are made.
    pub(crate) panic: bool,
    /// Fail once a pull has read the whole log, after its applies committed.
    pub(crate) fail_caught_up: bool,
}

impl Apply for Receiver {
    async fn apply(
        &mut self,
        conn: &mut SqliteConnection,
        replica: &Replica,
        changes: &[Change],
    ) -> std::result::Result<Applied, ClientError> {
        let vault = self.vault.as_deref();
        let applied = apply_and_repair(conn, replica, changes, false, vault).await?;
        #[cfg(test)]
        self.suffer(conn, !changes.is_empty()).await?;
        Ok(applied)
    }

    async fn caught_up(
        &mut self,
        conn: &mut SqliteConnection,
        replica: &Replica,
    ) -> std::result::Result<Option<Applied>, ClientError> {
        #[cfg(test)]
        if std::mem::take(&mut self.faults.fail_caught_up) {
            return Err(application(EngineError::Other("injected failure".into())));
        }
        apply_and_repair(conn, replica, &[], true, self.vault.as_deref())
            .await
            .map(Some)
    }
}

#[cfg(test)]
impl Receiver {
    async fn suffer(
        &mut self,
        conn: &mut SqliteConnection,
        received: bool,
    ) -> std::result::Result<(), ClientError> {
        use sqlx::Connection;

        let hold = std::mem::take(&mut self.faults.hold_write_lock);
        if std::mem::take(&mut self.faults.end_transaction) || hold {
            let mut own = conn.begin().await?;
            sqlx::query("ROLLBACK").execute(&mut *own).await?;
            std::mem::forget(own);
        }
        if hold {
            sqlx::query("BEGIN IMMEDIATE").execute(&mut *conn).await?;
        }
        if received && self.faults.hang {
            std::future::pending::<()>().await;
        }
        assert!(!(received && self.faults.panic), "injected panic");
        if received && self.faults.fail_applies > 0 {
            self.faults.fail_applies -= 1;
            return Err(application(EngineError::Other("injected failure".into())));
        }
        Ok(())
    }
}

/// Applies `changes` (none, to retry held rows and collect local writes) and brings up to date
/// what this device derives from the rows that wrote, so no read sees a remote row without it.
/// With `give_way`, rows here that lose a clash with a held row are given up
/// ([`clashes::give_way`]) and the held rows tried again; that must happen only once a pull has
/// read the whole log, never between pages, because a clash decided on part of what another
/// device sent may be one its later changes already ended.
async fn apply_and_repair(
    conn: &mut SqliteConnection,
    replica: &Replica,
    changes: &[Change],
    give_way: bool,
    vault: Option<&Path>,
) -> std::result::Result<Applied, ClientError> {
    synced::watch_covers(conn).await.map_err(application)?;
    let mut applied = replica.apply(conn, changes).await?;
    let mut touched = applied.touched.clone();
    for _ in 0..SETTLE_ROUNDS {
        if !give_way
            || applied.held.is_empty()
            || !clashes::give_way(conn, replica, &applied.held)
                .await
                .map_err(application)?
        {
            break;
        }
        let again = replica.apply(conn, &[]).await?;
        touched.extend(again.touched.iter().cloned());
        merge_applied(&mut applied, again);
    }
    synced::repair(conn, &touched, vault)
        .await
        .map_err(application)?;
    Ok(applied)
}

/// How many books a sync hands back to fetch covers for at most.
const COVERS_TO_FETCH: i64 = 100;
/// A cover file younger than this is never swept: a cover is written before its row points at it.
const COVER_SETTLE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// What a sync hands back besides its report, read from the database as it stands once the sync
/// succeeded, so a sync that failed or was cancelled earlier, or a restart, loses none of it.
#[derive(Debug, Default)]
pub(crate) struct After {
    pub covers_to_fetch: Vec<i64>,
    pub refused_notes: Vec<i64>,
}

/// Removes cover files no book uses, and reads the books whose covers are to be fetched and the
/// notes whose paths are refused.
pub(crate) async fn after_sync(
    storage: &Storage,
    images_dir: &Path,
    vault: &Path,
) -> Result<After> {
    let used = storage.cover_file_names().await?;
    let notes = storage.note_files().await?;
    let images_dir = images_dir.to_owned();
    let vault = vault.to_owned();
    let refused_notes = tokio::task::spawn_blocking(move || {
        crate::images::sweep_unused(&images_dir, &used, COVER_SETTLE)?;
        Ok::<_, EngineError>(refused_notes(&vault, notes))
    })
    .await
    .map_err(|e| EngineError::Other(format!("sweeping covers: {e}")))??;
    Ok(After {
        covers_to_fetch: storage.covers_to_fetch(COVERS_TO_FETCH).await?,
        refused_notes,
    })
}

/// Notes whose path is never resolved to a file: not plain, outside the vault, or the same as
/// another note's but for ASCII case and sorting after it.
fn refused_notes(vault: &Path, mut notes: Vec<crate::storage::NoteFile>) -> Vec<i64> {
    notes.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    let mut seen = std::collections::HashSet::new();
    notes
        .into_iter()
        .filter(|n| {
            let twin = !seen.insert(n.file_path.to_ascii_lowercase());
            twin || crate::notes::note_file(vault, &n.file_path).is_err()
        })
        .map(|n| n.id)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn application(e: EngineError) -> ClientError {
    ClientError::Application(Box::new(e))
}

/// The engine's error inside a client error a step returned, or the client error as it is.
pub(crate) fn unwrap_application(e: ClientError) -> std::result::Result<EngineError, ClientError> {
    match e {
        ClientError::Application(inner) => match inner.downcast::<EngineError>() {
            Ok(engine) => Ok(*engine),
            Err(other) => Ok(EngineError::Other(format!("sync: {other}"))),
        },
        other => Err(other),
    }
}

/// What arrived from another device, received outside a sync as a test delivers it.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct Received {
    pub applied: Applied,
    /// Books with a cover URL and no file, as a sync reports them.
    pub covers_to_fetch: Vec<i64>,
}

/// One page of changes from another device, applied as a sync's pull applies it, in a
/// transaction of its own.
#[cfg(test)]
pub(crate) async fn receive(
    storage: &Storage,
    replica: &Replica,
    changes: &[Change],
) -> Result<Received> {
    let mut receiver = Receiver::default();
    in_transaction(storage, async |conn| {
        receiver.apply(conn, replica, changes).await
    })
    .await
}

/// What a sync does once its pull has read the whole log.
#[cfg(test)]
pub(crate) async fn settle_clashes(storage: &Storage, replica: &Replica) -> Result<Received> {
    let mut receiver = Receiver::default();
    in_transaction(storage, async |conn| {
        Ok(receiver.caught_up(conn, replica).await?.unwrap_or_default())
    })
    .await
}

#[cfg(test)]
async fn in_transaction(
    storage: &Storage,
    step: impl AsyncFnOnce(&mut SqliteConnection) -> std::result::Result<Applied, ClientError>,
) -> Result<Received> {
    use sqlx::Connection;

    let mut conn = storage.pool().acquire().await?;
    let mut tx = conn.begin().await?;
    let applied = match step(&mut tx).await {
        Ok(applied) => applied,
        Err(e) => {
            return Err(unwrap_application(e).unwrap_or_else(|e| EngineError::Other(e.to_string())));
        }
    };
    tx.commit().await?;
    drop(conn);
    let covers_to_fetch = storage.covers_to_fetch(COVERS_TO_FETCH).await?;
    Ok(Received {
        applied,
        covers_to_fetch,
    })
}

pub(crate) fn kernel(e: fongo_sync_sqlite::Error) -> EngineError {
    match e {
        fongo_sync_sqlite::Error::Sql(e) => EngineError::Db(e),
        other => EngineError::Other(format!("sync: {other}")),
    }
}

pub(crate) mod http;
pub(crate) mod identity;
mod session;

pub(crate) use session::Slot;
pub use session::{
    RecoveryCode, SyncConflict, SyncError, SyncOutcome, SyncRow, SyncState, SyncStatus,
};

#[cfg(test)]
mod clash_tests;
#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod test_server;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use fongo_sync_sqlite::Key;
    use fongo_sync_sqlite::core::Change;
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

    type Side<'a> = (&'a Replica, &'a Storage);

    async fn installed_pair() -> ((Replica, Storage), (Replica, Storage)) {
        let (a, b) = (fresh().await, fresh().await);
        let (ra, rb) = (replica().unwrap(), replica().unwrap());
        for (s, r) in [(&a, &ra), (&b, &rb)] {
            r.install(&mut s.pool().acquire().await.unwrap())
                .await
                .unwrap();
        }
        ((ra, a), (rb, b))
    }

    /// What `side` would send now.
    async fn outbox(side: Side<'_>) -> fongo_sync_sqlite::Outbox {
        let mut conn = side.1.pool().acquire().await.unwrap();
        side.0.collect(&mut conn).await.unwrap();
        side.0.outbox(&mut conn, 100_000).await.unwrap()
    }

    /// Sends what `from` wrote to `to`, through [`receive`]. A side with nothing of its own to
    /// send has nothing after receiving either: no repair of what arrived is sent back.
    async fn send(from: Side<'_>, to: Side<'_>) -> Option<Received> {
        let out = outbox(from).await;
        if out.changes.is_empty() {
            return None;
        }
        let idle = outbox(to).await.changes.is_empty();
        let received = receive(to.1, to.0, &out.changes).await.unwrap();
        assert!(
            received.applied.held.is_empty(),
            "held back: {:?}",
            received.applied.held
        );
        if idle {
            let back = outbox(to).await.changes;
            assert!(back.is_empty(), "sent back: {back:#?}");
        }
        let mut conn = from.1.pool().acquire().await.unwrap();
        from.0.acknowledge(&mut conn, out.through).await.unwrap();
        Some(received)
    }

    /// Each side receives what the other wrote before either has seen anything from the other.
    async fn cross(a: Side<'_>, b: Side<'_>) {
        let (to_b, to_a) = (outbox(a).await, outbox(b).await);
        for (from, to, out) in [(a, b, to_b), (b, a, to_a)] {
            let received = receive(to.1, to.0, &out.changes).await.unwrap();
            assert!(received.applied.held.is_empty());
            let mut conn = from.1.pool().acquire().await.unwrap();
            from.0.acknowledge(&mut conn, out.through).await.unwrap();
        }
    }

    /// Sends both ways until neither side has anything to send, then checks that one more collect
    /// on each finds nothing: what a repair wrote is either local or already agreed.
    async fn exchange(a: Side<'_>, b: Side<'_>) {
        for _ in 0..6 {
            let there = send(a, b).await;
            let back = send(b, a).await;
            if there.is_none() && back.is_none() {
                assert!(outbox(a).await.changes.is_empty());
                assert!(outbox(b).await.changes.is_empty());
                return;
            }
        }
        panic!("the two libraries kept sending changes");
    }

    /// What a reader sees, without the local ids that differ between devices.
    async fn view(conn: &mut SqliteConnection) -> Vec<String> {
        let mut seen = Vec::new();
        for sql in [
            "SELECT 'book ' || title || ' ' || COALESCE(isbn_13, '-') FROM books",
            "SELECT 'highlight ' || b.title || ' ' || h.text || ' ' || h.identity_hash || ' '
                    || COALESCE(h.annotation, '-')
               FROM highlights h JOIN books b ON b.id = h.book_id",
            "SELECT 'tag ' || b.title || ' ' || t.tag || ' ' || t.source
               FROM book_tags t JOIN books b ON b.id = t.book_id",
            "SELECT 'note ' || n.file_path || ' ' || n.title || ' ' || b.body
               FROM notes n JOIN note_bodies b ON b.note_id = n.id",
            "SELECT 'rating ' || n.file_path || ' ' || s.name || ' ' || r.value
               FROM review_ratings r
               JOIN notes n ON n.id = r.note_id
               JOIN rating_scales s ON s.id = r.scale_id",
            "SELECT 'scale ' || name || ' ' || is_default FROM rating_scales",
            "SELECT 'map ' || s.name || ' ' || m.value || ' ' || m.goodreads
               FROM rating_map m JOIN rating_scales s ON s.id = m.scale_id",
        ] {
            let mut rows: Vec<String> =
                sqlx::query_scalar(sql).fetch_all(&mut *conn).await.unwrap();
            rows.sort();
            seen.extend(rows);
        }
        seen
    }

    #[tokio::test]
    async fn two_libraries_exchange_a_book_a_highlight_a_review_and_a_tag() {
        use crate::book::Book;
        use crate::storage::NewHighlight;

        let ((ra, a), (rb, b)) = installed_pair().await;

        let book = a
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    isbn_13: Some("9780141439549".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let passage = "Who can quit young lives after being long in company with them?";
        a.insert_highlight(
            book,
            &NewHighlight {
                text: passage.into(),
                chapter: Some("Finale".into()),
                page: Some(838),
                pos0: Some("/body/DocFragment[90]/p[1]/text().0".into()),
                pos1: None,
                ko_datetime: Some("2026-01-01 10:00:00".into()),
                ko_datetime_updated: None,
                color: None,
                note: None,
                source: "koreader".into(),
            },
        )
        .await
        .unwrap()
        .unwrap();
        let review = a
            .insert_note(
                crate::storage::NewNoteMeta {
                    book_id: Some(book),
                    reading_id: None,
                    highlight_id: None,
                    page: None,
                    location: None,
                    file_path: "Middlemarch/review.md",
                    title: "Middlemarch",
                    kind: "review",
                },
                "Her full nature spent itself in channels which had no great name on the earth.",
                &[],
            )
            .await
            .unwrap();
        let scale = a.rating_scale_by_name("default").await.unwrap().unwrap();
        a.set_review_rating(review, &scale, 4.0).await.unwrap();
        sqlx::query(
            "INSERT INTO book_tags (book_id, tag, source) VALUES (?1, 'to-read', 'goodreads')",
        )
        .bind(book)
        .execute(a.pool())
        .await
        .unwrap();

        exchange((&ra, &a), (&rb, &b)).await;
        let on_b: i64 = sqlx::query_scalar("SELECT id FROM highlights WHERE text = ?1")
            .bind(passage)
            .fetch_one(b.pool())
            .await
            .unwrap();
        b.set_annotation(on_b, Some("the last page")).await.unwrap();

        exchange((&ra, &a), (&rb, &b)).await;
        let seen = view(&mut a.pool().acquire().await.unwrap()).await;
        assert_eq!(seen, view(&mut b.pool().acquire().await.unwrap()).await);
        assert!(
            seen.contains(&"book Middlemarch 9780141439549".to_string()),
            "{seen:#?}"
        );
        assert!(
            seen.iter().any(|row| row.ends_with(" the last page")),
            "{seen:#?}"
        );
        assert!(
            seen.contains(&"tag Middlemarch to-read goodreads".to_string()),
            "{seen:#?}"
        );
        assert!(
            seen.contains(
                &"note Middlemarch/review.md Middlemarch Her full nature spent itself in channels \
                  which had no great name on the earth."
                    .to_string()
            ),
            "{seen:#?}"
        );
        assert!(
            seen.contains(&"rating Middlemarch/review.md default 4.0".to_string()),
            "{seen:#?}"
        );
        assert_eq!(
            seen.iter()
                .filter(|row| row.starts_with("scale default "))
                .count(),
            1
        );
    }

    async fn id_of(s: &Storage, title: &str) -> i64 {
        sqlx::query_scalar("SELECT id FROM books WHERE title = ?")
            .bind(title)
            .fetch_one(s.pool())
            .await
            .unwrap()
    }

    async fn sort_keys(s: &Storage, book: i64) -> (Option<String>, Option<String>) {
        sqlx::query_as("SELECT sort_title, sort_author FROM books WHERE id = ?")
            .bind(book)
            .fetch_one(s.pool())
            .await
            .unwrap()
    }

    fn middlemarch() -> crate::book::Book {
        crate::book::Book {
            title: Some("Middlemarch".into()),
            authors: vec!["George Eliot".into()],
            isbn_13: Some("9780141439549".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_remote_book_files_and_searches_as_one_made_here() {
        use crate::book::Book;
        use crate::storage::{BookFilter, LibraryScope, NewHighlight, SearchSource, Source};

        let ((ra, a), (rb, b)) = installed_pair().await;
        let on_a = a.upsert_book(&middlemarch(), None).await.unwrap();
        a.insert_highlight(
            on_a,
            &NewHighlight {
                text: "Who can quit young lives after being long in company with them?".into(),
                chapter: Some("Finale".into()),
                page: Some(838),
                pos0: None,
                pos1: None,
                ko_datetime: None,
                ko_datetime_updated: None,
                color: None,
                note: None,
                source: "koreader".into(),
            },
        )
        .await
        .unwrap()
        .unwrap();
        exchange((&ra, &a), (&rb, &b)).await;

        let on_b = id_of(&b, "Middlemarch").await;
        let here = fresh().await;
        let made_here = here.upsert_book(&middlemarch(), None).await.unwrap();
        let keys = sort_keys(&b, on_b).await;
        assert_eq!(keys, sort_keys(&here, made_here).await);
        assert!(keys.0.is_some(), "{keys:?}");

        let found = b
            .search_books("middlemarch", &BookFilter::default(), LibraryScope::All, 10)
            .await
            .unwrap();
        assert_eq!(found.iter().map(|m| m.book_id).collect::<Vec<_>>(), [on_b]);
        let marks = b
            .search_marks("young lives", Some(SearchSource::Highlight), None, 10)
            .await
            .unwrap();
        assert_eq!(marks.len(), 1);

        a.enrich_book(
            on_a,
            &Book {
                title: Some("The Annotated Middlemarch".into()),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();
        exchange((&ra, &a), (&rb, &b)).await;
        let moved = sort_keys(&b, on_b).await;
        assert_ne!(moved, keys);
        assert_eq!(moved, sort_keys(&a, on_a).await);
        let found = b
            .search_books("annotated", &BookFilter::default(), LibraryScope::All, 10)
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
    }

    /// Titles in the order a reader is shown.
    async fn list_order(s: &Storage) -> Vec<String> {
        use crate::lists::{EntryShow, EntrySort};

        let entries = s
            .list_entries(list_id(s).await, EntryShow::All, EntrySort::Yours)
            .await
            .unwrap();
        assert!(
            entries
                .iter()
                .enumerate()
                .all(|(i, e)| e.position == i as i64)
        );
        entries
            .into_iter()
            .map(|e| e.book.title.unwrap_or_default())
            .collect()
    }

    async fn stored_positions(s: &Storage) -> Vec<i64> {
        sqlx::query_scalar("SELECT position FROM book_list_entries ORDER BY position")
            .fetch_all(s.pool())
            .await
            .unwrap()
    }

    fn dense(positions: &[i64]) -> bool {
        positions.iter().copied().eq(0..positions.len() as i64)
    }

    async fn list_id(s: &Storage) -> i64 {
        sqlx::query_scalar("SELECT id FROM book_lists")
            .fetch_one(s.pool())
            .await
            .unwrap()
    }

    const ELIOT: [&str; 6] = [
        "Middlemarch",
        "Adam Bede",
        "Romola",
        "Silas Marner",
        "Daniel Deronda",
        "Felix Holt",
    ];

    /// Every book of `ELIOT` made on `s`, those in `listed` on one list in that order.
    async fn eliot_list(s: &Storage, made_elsewhere: &[&str], listed: &[&str]) -> i64 {
        use crate::book::Book;

        for title in ELIOT.iter().filter(|t| !made_elsewhere.contains(t)) {
            let book = Book {
                title: Some((*title).into()),
                ..Default::default()
            };
            s.upsert_book(&book, None).await.unwrap();
        }
        let list = s.create_list("Eliot", None).await.unwrap().id;
        for title in listed {
            s.add_to_list(list, id_of(s, title).await, None)
                .await
                .unwrap();
        }
        list
    }

    #[tokio::test]
    async fn two_devices_reordering_one_list_end_in_one_order() {
        use crate::book::Book;

        let ((ra, a), (rb, b)) = installed_pair().await;
        // Made on the other side, so local ids order the two appends below oppositely.
        b.upsert_book(
            &Book {
                title: Some("Felix Holt".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        let list = eliot_list(&a, &["Felix Holt"], &ELIOT[..4]).await;
        exchange((&ra, &a), (&rb, &b)).await;
        let list_b = list_id(&b).await;
        assert_eq!(list_order(&b).await, ELIOT[..4]);

        a.add_to_list(list, id_of(&a, "Daniel Deronda").await, None)
            .await
            .unwrap();
        b.add_to_list(list_b, id_of(&b, "Felix Holt").await, None)
            .await
            .unwrap();
        // Both appends take the last place in the same second; only the tie-break orders them.
        for s in [&a, &b] {
            sqlx::query("UPDATE book_list_entries SET added_at = 1700000000 WHERE position = 4")
                .execute(s.pool())
                .await
                .unwrap();
        }
        cross((&ra, &a), (&rb, &b)).await;
        let order = list_order(&a).await;
        assert_eq!(order, list_order(&b).await);
        assert_eq!(order.len(), 6, "{order:?}");
        exchange((&ra, &a), (&rb, &b)).await;
        assert_eq!(stored_positions(&b).await, [0, 1, 2, 3, 4, 4]);

        a.move_in_list(list, id_of(&a, "Adam Bede").await, 0)
            .await
            .unwrap();
        exchange((&ra, &a), (&rb, &b)).await;
        assert!(dense(&stored_positions(&a).await));
        assert_eq!(stored_positions(&a).await, stored_positions(&b).await);
        assert_eq!(list_order(&a).await[0], "Adam Bede");
        assert_eq!(list_order(&a).await, list_order(&b).await);

        a.move_in_list(list, id_of(&a, "Silas Marner").await, 0)
            .await
            .unwrap();
        b.move_in_list(list_b, id_of(&b, "Middlemarch").await, 5)
            .await
            .unwrap();
        cross((&ra, &a), (&rb, &b)).await;
        let order = list_order(&a).await;
        assert_eq!(order, list_order(&b).await);
        assert_eq!(order.len(), 6, "{order:?}");
        exchange((&ra, &a), (&rb, &b)).await;
        assert_eq!(list_order(&b).await, order);

        a.delete_book(id_of(&a, "Romola").await).await.unwrap();
        exchange((&ra, &a), (&rb, &b)).await;
        let order = list_order(&a).await;
        assert_eq!(order, list_order(&b).await);
        assert_eq!(order.len(), 5, "{order:?}");
    }

    /// Changes `side` would send, marked as delivered.
    async fn take(side: Side<'_>) -> Vec<Change> {
        let out = outbox(side).await;
        let mut conn = side.1.pool().acquire().await.unwrap();
        side.0.acknowledge(&mut conn, out.through).await.unwrap();
        out.changes
    }

    #[tokio::test]
    async fn a_move_that_arrives_in_two_parts_ends_as_the_reader_made_it() {
        let ((ra, a), (rb, b)) = installed_pair().await;
        let list = eliot_list(&a, &[], &ELIOT).await;
        exchange((&ra, &a), (&rb, &b)).await;

        a.move_in_list(list, id_of(&a, "Daniel Deronda").await, 0)
            .await
            .unwrap();
        let made = list_order(&a).await;
        let changes = take((&ra, &a)).await;
        assert!(changes.len() > 1, "{changes:?}");
        let (first, rest) = changes.split_at(changes.len() / 2);
        receive(&b, &rb, first).await.unwrap();
        assert!(take((&rb, &b)).await.is_empty());
        receive(&b, &rb, rest).await.unwrap();
        exchange((&ra, &a), (&rb, &b)).await;
        assert_eq!(list_order(&a).await, made);
        assert_eq!(list_order(&b).await, made);
    }

    async fn count(s: &Storage, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(s.pool()).await.unwrap()
    }

    #[tokio::test]
    async fn a_remote_note_is_found_by_title_then_by_its_body_and_leaves_no_index_when_removed() {
        use crate::storage::SearchSource;

        let ((ra, a), (rb, b)) = installed_pair().await;
        let note: i64 = sqlx::query_scalar(
            "INSERT INTO notes (file_path, title, kind, created_at, last_modified)
             VALUES ('Middlemarch/prelude.md', 'Prelude', 'note', 1, 1) RETURNING id",
        )
        .fetch_one(a.pool())
        .await
        .unwrap();
        exchange((&ra, &a), (&rb, &b)).await;
        let found = b
            .search_marks("prelude", Some(SearchSource::Note), None, 10)
            .await
            .unwrap();
        assert_eq!(found.len(), 1, "{found:?}");

        let body = "Who that cares much to know the history of man has not dwelt on the life of \
                    Saint Theresa? See [[Finale]].";
        a.refresh_note_body(note, "Prelude", body).await.unwrap();
        exchange((&ra, &a), (&rb, &b)).await;
        let found = b
            .search_marks("saint theresa", Some(SearchSource::Note), None, 10)
            .await
            .unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        let on_b: i64 = count(&b, "SELECT id FROM notes").await;
        let parts = "SELECT parts || ' ' || latest_part_at FROM notes";
        let stamps: (String, String) = (
            sqlx::query_scalar(parts).fetch_one(a.pool()).await.unwrap(),
            sqlx::query_scalar(parts).fetch_one(b.pool()).await.unwrap(),
        );
        assert_eq!(stamps.0, stamps.1);
        let links: Vec<String> =
            sqlx::query_scalar("SELECT target_title FROM note_links WHERE from_note = ?")
                .bind(on_b)
                .fetch_all(b.pool())
                .await
                .unwrap();
        assert_eq!(links, ["Finale"]);

        a.delete_note(note).await.unwrap();
        exchange((&ra, &a), (&rb, &b)).await;
        assert_eq!(count(&b, "SELECT count(*) FROM notes").await, 0);
        assert_eq!(count(&b, "SELECT count(*) FROM notes_fts").await, 0);
    }

    #[tokio::test]
    async fn a_changed_cover_url_is_handed_back_and_a_removed_books_cover_file_goes() {
        use crate::book::Book;
        use crate::storage::Source;

        let ((ra, a), (rb, b)) = installed_pair().await;
        let on_a = a
            .upsert_book(
                &Book {
                    cover_url: Some("https://covers.example/1.jpg".into()),
                    ..middlemarch()
                },
                None,
            )
            .await
            .unwrap();
        let got = send((&ra, &a), (&rb, &b)).await.unwrap();
        let on_b = id_of(&b, "Middlemarch").await;
        assert_eq!(got.covers_to_fetch, [on_b]);
        exchange((&ra, &a), (&rb, &b)).await;

        let dir = tempfile::tempdir().unwrap();
        let cover = dir.path().join(cover_name(1));
        let thumb = crate::images::thumb_path_of(&cover);
        std::fs::write(&cover, b"cover").unwrap();
        std::fs::write(&thumb, b"thumb").unwrap();
        sqlx::query("UPDATE books SET cover_path = ? WHERE id = ?")
            .bind(cover.display().to_string())
            .bind(on_b)
            .execute(b.pool())
            .await
            .unwrap();

        let edit = |book: Book| {
            let a = &a;
            async move {
                a.enrich_book(on_a, &book, Some(Source::User))
                    .await
                    .unwrap()
            }
        };
        edit(Book {
            publisher: Some("Blackwood".into()),
            ..Default::default()
        })
        .await;
        let got = send((&ra, &a), (&rb, &b)).await.unwrap();
        assert!(got.covers_to_fetch.is_empty());
        edit(Book {
            cover_url: Some("https://covers.example/2.jpg".into()),
            ..Default::default()
        })
        .await;
        let got = send((&ra, &a), (&rb, &b)).await.unwrap();
        assert_eq!(got.covers_to_fetch, [on_b]);
        exchange((&ra, &a), (&rb, &b)).await;

        a.delete_book(on_a).await.unwrap();
        exchange((&ra, &a), (&rb, &b)).await;
        sweep(&b, dir.path()).await;
        assert_eq!(count(&b, "SELECT count(*) FROM books").await, 0);
        assert!(!cover.exists());
        assert!(!thumb.exists());
    }

    /// A cover file name as the engine generates them.
    fn cover_name(n: u8) -> String {
        format!("{}.jpg", format!("{n:02x}").repeat(32))
    }

    /// What a successful sync does to `dir`'s cover files, with every file there old enough.
    async fn sweep(s: &Storage, dir: &std::path::Path) {
        let old = std::time::SystemTime::now() - 2 * COVER_SETTLE;
        for entry in std::fs::read_dir(dir).unwrap() {
            let file = std::fs::File::options()
                .write(true)
                .open(entry.unwrap().path())
                .unwrap();
            file.set_modified(old).unwrap();
        }
        let used = s.cover_file_names().await.unwrap();
        crate::images::sweep_unused(dir, &used, COVER_SETTLE).unwrap();
    }

    async fn set_cover_path(s: &Storage, book: i64, path: &std::path::Path) {
        sqlx::query("UPDATE books SET cover_path = ?, cover_width = 600 WHERE id = ?")
            .bind(path.display().to_string())
            .bind(book)
            .execute(s.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_book_row_given_up_when_two_become_one_hands_on_its_cover() {
        use crate::book::Book;

        for cover_on_isbn_10 in [true, false] {
            let ((ra, a), (rb, b)) = installed_pair().await;
            a.upsert_book(
                &Book {
                    isbn_10: Some("0141439548".into()),
                    ..middlemarch()
                },
                None,
            )
            .await
            .unwrap();
            let by_isbn_10 = b
                .upsert_book(
                    &Book {
                        title: Some("Middlemarch".into()),
                        isbn_10: Some("0141439548".into()),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .unwrap();
            let by_isbn_13 = b
                .upsert_book(
                    &Book {
                        title: Some("Middlemarch".into()),
                        isbn_13: Some("9780141439549".into()),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let cover = dir.path().join(cover_name(1));
            std::fs::write(&cover, b"cover").unwrap();
            let holder = if cover_on_isbn_10 {
                by_isbn_10
            } else {
                by_isbn_13
            };
            set_cover_path(&b, holder, &cover).await;

            let out = outbox((&ra, &a)).await;
            receive(&b, &rb, &out.changes).await.unwrap();
            sweep(&b, dir.path()).await;
            let rows: Vec<(Option<String>, Option<i64>)> =
                sqlx::query_as("SELECT cover_path, cover_width FROM books")
                    .fetch_all(b.pool())
                    .await
                    .unwrap();
            assert_eq!(
                rows,
                [(Some(cover.display().to_string()), Some(600))],
                "cover on the isbn-10 row: {cover_on_isbn_10}"
            );
            assert!(cover.exists());
        }
    }

    /// Two rows on `b` for the book `a` holds with both ISBNs, one by each.
    async fn split_book(b: &Storage, title: &str, isbn_10: &str, isbn_13: &str) -> [i64; 2] {
        use crate::book::Book;

        let mut rows = [0; 2];
        for (row, book) in rows.iter_mut().zip([
            Book {
                title: Some(title.into()),
                isbn_10: Some(isbn_10.into()),
                ..Default::default()
            },
            Book {
                title: Some(title.into()),
                isbn_13: Some(isbn_13.into()),
                ..Default::default()
            },
        ]) {
            *row = b.upsert_book(&book, None).await.unwrap();
        }
        rows
    }

    async fn cover_of(s: &Storage, title: &str) -> Option<String> {
        sqlx::query_scalar("SELECT cover_path FROM books WHERE title = ?")
            .bind(title)
            .fetch_one(s.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_row_that_stays_when_two_become_one_keeps_its_own_cover_and_the_other_file_goes() {
        use crate::book::Book;

        let ((ra, a), (rb, b)) = installed_pair().await;
        a.upsert_book(
            &Book {
                isbn_10: Some("0141439548".into()),
                ..middlemarch()
            },
            None,
        )
        .await
        .unwrap();
        let rows = split_book(&b, "Middlemarch", "0141439548", "9780141439549").await;
        let dir = tempfile::tempdir().unwrap();
        let files = [
            dir.path().join(cover_name(10)),
            dir.path().join(cover_name(13)),
        ];
        for (row, file) in rows.iter().zip(&files) {
            std::fs::write(file, b"cover").unwrap();
            set_cover_path(&b, *row, file).await;
        }

        let out = outbox((&ra, &a)).await;
        receive(&b, &rb, &out.changes).await.unwrap();
        sweep(&b, dir.path()).await;
        let stays = id_of(&b, "Middlemarch").await;
        let (kept, gone) = if stays == rows[0] {
            (&files[0], &files[1])
        } else {
            (&files[1], &files[0])
        };
        assert_eq!(
            cover_of(&b, "Middlemarch").await,
            Some(kept.display().to_string())
        );
        assert!(kept.exists());
        assert!(!gone.exists());
    }

    #[tokio::test]
    async fn two_books_that_each_become_one_in_one_apply_keep_their_own_covers() {
        use crate::book::Book;

        // Which row of each pair is given up depends on random ids, so try until both carry.
        for _ in 0..64 {
            let ((ra, a), (rb, b)) = installed_pair().await;
            let pairs = [
                ("Middlemarch", "0141439548", "9780141439549"),
                ("Daniel Deronda", "0140434275", "9780140434279"),
            ];
            for (title, isbn_10, isbn_13) in pairs {
                let book = Book {
                    title: Some(title.into()),
                    isbn_10: Some(isbn_10.into()),
                    isbn_13: Some(isbn_13.into()),
                    ..Default::default()
                };
                a.upsert_book(&book, None).await.unwrap();
            }
            let dir = tempfile::tempdir().unwrap();
            let mut covered = Vec::new();
            for (title, isbn_10, isbn_13) in pairs {
                let rows = split_book(&b, title, isbn_10, isbn_13).await;
                let file = dir.path().join(cover_name(covered.len() as u8));
                std::fs::write(&file, b"cover").unwrap();
                set_cover_path(&b, rows[0], &file).await;
                covered.push((title, rows[0], file));
            }

            let out = outbox((&ra, &a)).await;
            receive(&b, &rb, &out.changes).await.unwrap();
            sweep(&b, dir.path()).await;
            let mut carried = 0;
            for (title, row, file) in &covered {
                assert_eq!(
                    cover_of(&b, title).await,
                    Some(file.display().to_string()),
                    "{title}"
                );
                assert!(file.exists());
                carried += usize::from(id_of(&b, title).await != *row);
            }
            if carried == covered.len() {
                return;
            }
        }
        panic!("no apply gave up both covered rows");
    }

    #[tokio::test]
    async fn a_cover_file_goes_only_with_an_apply_that_commits() {
        use sqlx::Connection;

        let ((ra, a), (rb, b)) = installed_pair().await;
        let on_a = a.upsert_book(&middlemarch(), None).await.unwrap();
        exchange((&ra, &a), (&rb, &b)).await;
        let dir = tempfile::tempdir().unwrap();
        let cover = dir.path().join(cover_name(1));
        std::fs::write(&cover, b"cover").unwrap();
        set_cover_path(&b, id_of(&b, "Middlemarch").await, &cover).await;
        a.delete_book(on_a).await.unwrap();
        let changes = take((&ra, &a)).await;

        let mut conn = b.pool().acquire().await.unwrap();
        let mut tx = conn.begin().await.unwrap();
        Receiver::default()
            .apply(&mut tx, &rb, &changes)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        drop(conn);
        sweep(&b, dir.path()).await;
        assert!(cover.exists());
        assert_eq!(count(&b, "SELECT count(*) FROM books").await, 1);

        receive(&b, &rb, &changes).await.unwrap();
        sweep(&b, dir.path()).await;
        assert_eq!(count(&b, "SELECT count(*) FROM books").await, 0);
        assert!(!cover.exists());
    }

    #[tokio::test]
    async fn a_remote_delete_keeps_a_cover_file_another_book_uses() {
        use crate::book::Book;

        let ((ra, a), (rb, b)) = installed_pair().await;
        let gone = a.upsert_book(&middlemarch(), None).await.unwrap();
        a.upsert_book(
            &Book {
                title: Some("Middlemarch".into()),
                isbn_13: Some("9780199536757".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        exchange((&ra, &a), (&rb, &b)).await;
        let dir = tempfile::tempdir().unwrap();
        let cover = dir.path().join(cover_name(1));
        std::fs::write(&cover, b"cover").unwrap();
        let on_b: Vec<i64> = sqlx::query_scalar("SELECT id FROM books")
            .fetch_all(b.pool())
            .await
            .unwrap();
        for book in on_b {
            set_cover_path(&b, book, &cover).await;
        }

        a.delete_book(gone).await.unwrap();
        exchange((&ra, &a), (&rb, &b)).await;
        sweep(&b, dir.path()).await;
        assert_eq!(count(&b, "SELECT count(*) FROM books").await, 1);
        assert!(cover.exists());
    }
}
