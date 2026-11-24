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
pub(crate) struct Cover {
    cover_url: Option<String>,
    cover_path: Option<String>,
    cover_thumb_path: Option<String>,
    cover_width: Option<i64>,
    cover_height: Option<i64>,
    cover_accent: Option<i64>,
}

/// Each book's cover before an apply, for books with a cover URL or file.
pub(crate) type Covers = HashMap<i64, Cover>;

pub(crate) async fn covers(conn: &mut SqliteConnection) -> Result<Covers> {
    let rows = sqlx::query(
        "SELECT id, cover_url, cover_path, cover_thumb_path, cover_width, cover_height,
                cover_accent
           FROM books WHERE cover_url IS NOT NULL OR cover_path IS NOT NULL",
    )
    .fetch_all(conn)
    .await?;
    rows.iter()
        .map(|r| {
            let cover = Cover {
                cover_url: r.try_get("cover_url")?,
                cover_path: r.try_get("cover_path")?,
                cover_thumb_path: r.try_get("cover_thumb_path")?,
                cover_width: r.try_get("cover_width")?,
                cover_height: r.try_get("cover_height")?,
                cover_accent: r.try_get("cover_accent")?,
            };
            Ok((r.try_get("id")?, cover))
        })
        .collect()
}

/// A row the kernel gave up when two books became one hands its file to the row that stays, as
/// `merge_books` does, unless that row has its own. Returns whether it did.
async fn carry_cover(conn: &mut SqliteConnection, to: i64, cover: &Cover) -> Result<bool> {
    let carried = sqlx::query(
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
    Ok(carried.rows_affected() > 0)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Repaired {
    /// Books whose cover URL changed to one this device has not fetched.
    pub covers_to_fetch: Vec<i64>,
    /// Cover files of removed books that no book here uses any more.
    pub orphaned_covers: Vec<String>,
    /// Notes whose file path is not a markdown file inside the vault: kept as rows, never
    /// resolved to a file, and left out of the index.
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

/// `before` is [`covers`] read ahead of the apply that wrote `touched`; `vault` is where notes'
/// files live, to check their paths against.
pub(crate) async fn repair(
    conn: &mut SqliteConnection,
    touched: &[Touched],
    before: &Covers,
    vault: Option<&Path>,
) -> Result<Repaired> {
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
    let mut repaired = Repaired::default();
    for &id in &books {
        let before = before.get(&id).cloned().unwrap_or_default();
        let url: Option<Option<String>> =
            sqlx::query_scalar("SELECT cover_url FROM books WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?;
        if let Some(url) = url {
            super::books::refresh_sort_keys(conn, id).await?;
            if url.is_some() && url != before.cover_url {
                repaired.covers_to_fetch.push(id);
            }
            continue;
        }
        let Some(path) = before.cover_path.clone() else {
            continue;
        };
        let merged_into = touched
            .iter()
            .filter(|t| t.collection == "books" && t.deleted && ids(&t.key).first() == Some(&id))
            .find_map(|t| kept.get(&t.gid).filter(|&&to| to != id));
        if let Some(&to) = merged_into
            && carry_cover(conn, to, &before).await?
        {
            continue;
        }
        let others: i64 = sqlx::query_scalar("SELECT count(*) FROM books WHERE cover_path = ?")
            .bind(&path)
            .fetch_one(&mut *conn)
            .await?;
        if others == 0 && !repaired.orphaned_covers.contains(&path) {
            repaired.orphaned_covers.push(path);
        }
    }
    for &note in &notes {
        let path: Option<String> = sqlx::query_scalar("SELECT file_path FROM notes WHERE id = ?")
            .bind(note)
            .fetch_optional(&mut *conn)
            .await?;
        match path {
            Some(path) if !acceptable(vault, &path) => {
                super::notes::unindex_stored(conn, note).await?;
                repaired.refused_notes.push(note);
            }
            _ => super::notes::reindex_stored(conn, note).await?,
        }
    }
    Ok(repaired)
}
