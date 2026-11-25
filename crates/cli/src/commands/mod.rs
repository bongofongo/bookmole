pub mod activity;
pub mod book;
pub mod calibre;
pub mod cards;
pub mod config;
pub mod covers;
pub mod enrich;
pub mod find;
pub mod goodreads;
pub mod ko;
pub mod note;
pub mod rating;
pub mod reflect;
pub mod search;
pub mod sort_keys;
pub mod sync;

use anyhow::{Result, bail};
use bookmole::{Book, Engine, NoteRecord, NoteScope};

use crate::render;

/// Syncs the note index with the vault before reading it. Never fatal: a failed sweep falls back
/// to the existing index.
pub(crate) async fn catch_up(engine: &Engine) {
    if let Err(e) = engine.reconcile_vault().await {
        tracing::warn!(error = %e, "could not reconcile the vault");
    }
}

/// Resolves id | ISBN | title fragment to exactly one book; an ambiguous selector lists candidates.
pub async fn resolve_one(engine: &Engine, selector: &str) -> Result<Book> {
    let mut candidates = engine.resolve_books(selector).await?;
    match candidates.len() {
        0 => bail!("no book matches '{selector}' — try `bookmole list`"),
        1 => Ok(candidates.remove(0)),
        _ => {
            let mut msg = format!("'{selector}' is ambiguous:\n");
            for b in &candidates {
                msg.push_str(&format!("  {}\n", render::book_line(b)));
            }
            msg.push_str("use the #id instead");
            bail!(msg)
        }
    }
}

pub async fn resolve_note(engine: &Engine, selector: &str) -> Result<NoteRecord> {
    if let Ok(id) = selector.parse::<i64>()
        && let Some(note) = engine.get_note(id).await?
    {
        return Ok(note);
    }
    let needle = selector.to_lowercase();
    let mut candidates: Vec<NoteRecord> = engine
        .list_notes(NoteScope::All, None)
        .await?
        .into_iter()
        .filter(|n| n.title.to_lowercase().contains(&needle))
        .collect();
    match candidates.len() {
        0 => bail!("no note matches '{selector}' — try `bookmole notes`"),
        1 => Ok(candidates.remove(0)),
        _ => {
            let mut msg = format!("'{selector}' is ambiguous:\n");
            for n in &candidates {
                msg.push_str(&format!("  #{:<4} {}\n", n.id, n.title));
            }
            msg.push_str("use the #id instead");
            bail!(msg)
        }
    }
}
