//! What this device derives from synced rows, brought up to date after another device's changes
//! land, inside the transaction that applied them. Nothing here writes a synced column: a write
//! after the apply would be sent with a newer clock than the changes still on their way.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use fongo_sync_sqlite::Touched;
use sqlx::{Row, SqliteConnection};

use crate::error::Result;

/// A book's cover as it stood before an apply.
#[derive(Debug, Clone, Default)]
struct Cover {
    cover_path: Option<String>,
    cover_thumb_path: Option<String>,
    cover_width: Option<i64>,
    cover_height: Option<i64>,
    cover_accent: Option<i64>,
}

/// Temporary triggers on this connection record a book's cover as it stood before the apply
/// removed the row or changed its cover URL, and the path of every note it removed or moved, so
/// the repair reads only the rows the apply touched. Made inside the apply's transaction and
/// dropped by [`repair`] in it.
const WATCH_COVERS: &[&str] = &[
    "CREATE TEMP TABLE IF NOT EXISTS bookmole_cover_before (
        id INTEGER PRIMARY KEY,
        url_changed INTEGER NOT NULL,
        cover_path TEXT, cover_thumb_path TEXT,
        cover_width INTEGER, cover_height INTEGER, cover_accent INTEGER)",
    "CREATE TEMP TRIGGER IF NOT EXISTS bookmole_cover_gone BEFORE DELETE ON main.books
     BEGIN
        INSERT OR IGNORE INTO bookmole_cover_before VALUES (old.id, 0, old.cover_path,
            old.cover_thumb_path, old.cover_width, old.cover_height, old.cover_accent);
     END",
    "CREATE TEMP TABLE IF NOT EXISTS bookmole_note_path_before (file_path TEXT NOT NULL)",
    "CREATE TEMP TRIGGER IF NOT EXISTS bookmole_note_gone BEFORE DELETE ON main.notes
     BEGIN
        INSERT INTO bookmole_note_path_before VALUES (old.file_path);
     END",
    "CREATE TEMP TRIGGER IF NOT EXISTS bookmole_note_moved BEFORE UPDATE OF file_path ON main.notes
     BEGIN
        INSERT INTO bookmole_note_path_before VALUES (old.file_path);
     END",
    "CREATE TEMP TRIGGER IF NOT EXISTS bookmole_cover_moved BEFORE UPDATE OF cover_url ON main.books
     WHEN old.cover_url IS NOT new.cover_url
     BEGIN
        INSERT OR IGNORE INTO bookmole_cover_before VALUES (old.id, 1, old.cover_path,
            old.cover_thumb_path, old.cover_width, old.cover_height, old.cover_accent);
     END",
];

/// Starts recording covers for [`repair`]; call in the apply's transaction, before the apply.
pub(crate) async fn watch_covers(conn: &mut SqliteConnection) -> Result<()> {
    for sql in WATCH_COVERS {
        sqlx::query(sql).execute(&mut *conn).await?;
    }
    Ok(())
}

/// The paths of notes the apply removed or moved, as [`watch_covers`] recorded them.
async fn paths_before(conn: &mut SqliteConnection) -> Result<Vec<String>> {
    let paths = sqlx::query_scalar("SELECT file_path FROM temp.bookmole_note_path_before")
        .fetch_all(&mut *conn)
        .await?;
    for sql in [
        "DROP TRIGGER IF EXISTS temp.bookmole_note_gone",
        "DROP TRIGGER IF EXISTS temp.bookmole_note_moved",
        "DROP TABLE IF EXISTS temp.bookmole_note_path_before",
    ] {
        sqlx::query(sql).execute(&mut *conn).await?;
    }
    Ok(paths)
}

/// What [`watch_covers`] recorded, by book, after which it records nothing more.
async fn covers_before(conn: &mut SqliteConnection) -> Result<HashMap<i64, (bool, Cover)>> {
    let rows = sqlx::query("SELECT * FROM temp.bookmole_cover_before")
        .fetch_all(&mut *conn)
        .await?;
    for sql in [
        "DROP TRIGGER IF EXISTS temp.bookmole_cover_gone",
        "DROP TRIGGER IF EXISTS temp.bookmole_cover_moved",
        "DROP TABLE IF EXISTS temp.bookmole_cover_before",
    ] {
        sqlx::query(sql).execute(&mut *conn).await?;
    }
    rows.iter()
        .map(|r| {
            let cover = Cover {
                cover_path: r.try_get("cover_path")?,
                cover_thumb_path: r.try_get("cover_thumb_path")?,
                cover_width: r.try_get("cover_width")?,
                cover_height: r.try_get("cover_height")?,
                cover_accent: r.try_get("cover_accent")?,
            };
            Ok((
                r.try_get("id")?,
                (r.try_get::<i64, _>("url_changed")? == 1, cover),
            ))
        })
        .collect()
}

/// A row the kernel gave up when two books became one hands its file to the row that stays, as
/// `merge_books` does, unless that row has its own; otherwise the file is left unused.
async fn carry_cover(conn: &mut SqliteConnection, to: i64, cover: &Cover) -> Result<()> {
    sqlx::query(
        "UPDATE books SET cover_path = ?1, cover_thumb_path = ?2, cover_width = ?3,
                cover_height = ?4, cover_accent = ?5
          WHERE id = ?6 AND cover_path IS NULL",
    )
    .bind(&cover.cover_path)
    .bind(&cover.cover_thumb_path)
    .bind(cover.cover_width)
    .bind(cover.cover_height)
    .bind(cover.cover_accent)
    .bind(to)
    .execute(conn)
    .await?;
    Ok(())
}

/// Notes whose stored file path is never resolved to a file. Kept as rows, left out of the index.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Repaired {
    pub refused_notes: Vec<i64>,
}

/// Whether a note's stored path may be resolved to a file: plain, and inside `vault` when one is
/// given.
fn acceptable(vault: Option<&Path>, file_path: &str) -> bool {
    crate::notes::note_path_is_plain(file_path)
        && vault.is_none_or(|v| crate::notes::note_file(v, file_path).is_ok())
}

/// The local ids in a key as the change-capture triggers write it: `[7]`, or `[3,7]`.
pub(super) fn ids(key: &str) -> Vec<i64> {
    serde_json::from_str::<Vec<serde_json::Value>>(key)
        .unwrap_or_default()
        .iter()
        .filter_map(serde_json::Value::as_i64)
        .collect()
}

fn keys_of<'t>(
    touched: &'t [Touched],
    collections: &'t [&str],
) -> impl Iterator<Item = Vec<i64>> + 't {
    touched
        .iter()
        .filter(|t| collections.contains(&t.collection.as_str()))
        .map(|t| ids(&t.key))
}

/// Brings what this device derives from synced rows in line after an apply that wrote
/// `touched`, which ran after [`watch_covers`]: sort keys; a cover file carried from a row a
/// merge gave up; a downloaded cover whose URL changed elsewhere forgotten here, so the book
/// reads as one whose cover is to be fetched and the old file as unused (a cover from a file on
/// this device is its own origin and stays); the notes index. `vault` is where
/// notes' files live, to check their paths against. Work is proportional to `touched`.
pub(crate) async fn repair(
    conn: &mut SqliteConnection,
    touched: &[Touched],
    vault: Option<&Path>,
) -> Result<Repaired> {
    let before = covers_before(conn).await?;
    let books: BTreeSet<i64> = keys_of(touched, &["books"])
        .filter_map(|k| k.first().copied())
        .collect();
    let notes: BTreeSet<i64> = keys_of(touched, &["notes", "note_bodies"])
        .filter_map(|k| k.first().copied())
        .collect();

    let mut kept = HashMap::new();
    for t in touched
        .iter()
        .filter(|t| t.collection == "books" && !t.deleted)
    {
        if let Some(&id) = ids(&t.key).first() {
            kept.insert(t.gid, id);
        }
    }
    for &id in &books {
        let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM books WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
        if exists.is_some() {
            super::books::refresh_sort_keys(conn, id).await?;
            let downloaded_cover_moved = before.get(&id).is_some_and(|(url_changed, cover)| {
                *url_changed
                    && cover
                        .cover_path
                        .as_deref()
                        .is_some_and(crate::images::downloaded)
            });
            if downloaded_cover_moved {
                forget_cover(conn, id).await?;
            }
            continue;
        }
        let Some((_, cover)) = before.get(&id).filter(|(_, c)| c.cover_path.is_some()) else {
            continue;
        };
        let merged_into = touched
            .iter()
            .filter(|t| t.collection == "books" && t.deleted && ids(&t.key).first() == Some(&id))
            .find_map(|t| kept.get(&t.gid).filter(|&&to| to != id));
        if let Some(&to) = merged_into {
            carry_cover(conn, to, cover).await?;
        }
    }
    // Every note sharing a path, by key, with one the apply wrote, removed or moved is decided
    // again, so who owns a file depends on the rows alone, not on the order they arrived in.
    let mut keys: BTreeSet<String> = paths_before(conn)
        .await?
        .iter()
        .map(|p| super::notes::path_key(p))
        .collect();
    let mut titles = super::notes::Titles::read();
    for &note in &notes {
        let path: Option<String> = sqlx::query_scalar("SELECT file_path FROM notes WHERE id = ?")
            .bind(note)
            .fetch_optional(&mut *conn)
            .await?;
        match path {
            Some(path) => {
                keys.insert(super::notes::path_key(&path));
            }
            None => super::notes::reindex_stored(conn, note, &mut titles).await?,
        }
    }
    let refused_notes =
        super::notes::settle_groups(conn, &keys, |path| acceptable(vault, path), &mut titles)
            .await?;
    Ok(Repaired { refused_notes })
}

/// Forgets the downloaded file of a book whose cover URL another device changed; only local
/// columns.
async fn forget_cover(conn: &mut SqliteConnection, id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE books SET cover_path = NULL, cover_thumb_path = NULL, cover_width = NULL,
                cover_height = NULL, cover_accent = NULL
          WHERE id = ? AND cover_path IS NOT NULL",
    )
    .bind(id)
    .execute(conn)
    .await?;
    Ok(())
}
