//! What this device derives from synced rows, brought up to date after another device's changes
//! land, inside the transaction that applied them.

use std::collections::{BTreeSet, HashMap};

use fongo_sync_sqlite::{Replica, Touched};
use sqlx::SqliteConnection;

use crate::error::Result;

/// Each book's `(cover_url, cover_path)` before an apply, for books with either.
pub(crate) type Covers = HashMap<i64, (Option<String>, Option<String>)>;

pub(crate) async fn covers(conn: &mut SqliteConnection) -> Result<Covers> {
    let rows: Vec<(i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, cover_url, cover_path FROM books
          WHERE cover_url IS NOT NULL OR cover_path IS NOT NULL",
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, url, path)| (id, (url, path)))
        .collect())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Repaired {
    /// Books whose cover URL changed to one this device has not fetched.
    pub covers_to_fetch: Vec<i64>,
    /// Cover files of removed books that no book here uses any more.
    pub orphaned_covers: Vec<String>,
}

/// The local ids in a key as the change-capture triggers write it: `[7]`, or `[3,7]`.
fn ids(key: &str) -> Vec<i64> {
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

/// `before` is [`covers`] read ahead of the apply that wrote `touched`.
pub(crate) async fn repair(
    conn: &mut SqliteConnection,
    replica: &Replica,
    touched: &[Touched],
    before: &Covers,
) -> Result<Repaired> {
    let books: BTreeSet<i64> = keys_of(touched, &["books"])
        .filter_map(|k| k.first().copied())
        .collect();
    let lists: BTreeSet<i64> = keys_of(touched, &["book_list_entries"])
        .filter_map(|k| k.first().copied())
        .collect();
    let notes: BTreeSet<i64> = keys_of(touched, &["notes", "note_bodies"])
        .filter_map(|k| k.first().copied())
        .collect();

    let mut repaired = Repaired::default();
    for &id in &books {
        let (url_before, path_before) = before.get(&id).cloned().unwrap_or_default();
        let url: Option<Option<String>> =
            sqlx::query_scalar("SELECT cover_url FROM books WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?;
        match url {
            Some(url) => {
                super::books::refresh_sort_keys(conn, id).await?;
                if url.is_some() && url != url_before {
                    repaired.covers_to_fetch.push(id);
                }
            }
            None => {
                let Some(path) = path_before else { continue };
                let others: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM books WHERE cover_path = ?")
                        .bind(&path)
                        .fetch_one(&mut *conn)
                        .await?;
                if others == 0 && !repaired.orphaned_covers.contains(&path) {
                    repaired.orphaned_covers.push(path);
                }
            }
        }
    }
    for &list in &lists {
        settle_list(conn, replica, list).await?;
    }
    for &note in &notes {
        super::notes::reindex_stored(conn, note).await?;
    }
    Ok(repaired)
}

/// Positions back to `0..n`, with ties between devices' entries broken by what every device
/// agrees on (the book's sync id, not its local one), so each device writes the same order and
/// none sends a correction back. `updated_at` is left alone: no one edited the list here.
async fn settle_list(conn: &mut SqliteConnection, replica: &Replica, list_id: i64) -> Result<()> {
    let mut entries = Vec::new();
    for (book_id, position, added_at) in super::lists::entries_of(&mut *conn, list_id).await? {
        let gid = replica
            .gid_of(&mut *conn, "books", book_id)
            .await
            .map_err(crate::sync::kernel)?;
        entries.push((position, added_at, gid, book_id));
    }
    entries.sort_unstable();
    let order: Vec<i64> = entries.iter().map(|e| e.3).collect();
    super::lists::renumber(conn, list_id, &order).await
}
