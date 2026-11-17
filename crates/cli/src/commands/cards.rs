use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bookmole::Engine;

pub async fn list(engine: &Engine, all: bool) -> Result<()> {
    let cards = engine.list_flashcards(all).await?;
    if cards.is_empty() {
        println!("no flashcard candidates — single-word KOReader highlights land here.");
        return Ok(());
    }
    for c in cards {
        let mark = if c.exported { "✓" } else { " " };
        let ctx = c.context.as_deref().unwrap_or("");
        println!(
            "{mark} #{:<4} {:<24} {ctx}  [{}]",
            c.id, c.word, c.book_title
        );
    }
    Ok(())
}

pub async fn export(
    engine: &Engine,
    out: Option<&Path>,
    all: bool,
    book: Option<&str>,
) -> Result<()> {
    let book_id = match book {
        Some(sel) => Some(
            super::resolve_one(engine, sel)
                .await?
                .id
                .context("a resolved book has no id")?,
        ),
        None => None,
    };
    let export = engine.export_anki(book_id, all).await?;
    if export.count == 0 {
        println!(
            "nothing written: every card had gone out in an earlier file. \
             --all writes them all again; Anki updates the notes it has \
             rather than adding copies."
        );
        return Ok(());
    }
    let out = out.map_or_else(|| PathBuf::from(&export.filename), Path::to_path_buf);
    std::fs::write(&out, &export.text)?;
    println!("{}", sent(export.count, &out));
    Ok(())
}

pub fn sent(count: usize, out: &Path) -> String {
    let cards = if count == 1 { "card" } else { "cards" };
    format!(
        "{count} {cards} -> {} (Anki: File > Import; importing it again updates, never duplicates)",
        out.display()
    )
}
