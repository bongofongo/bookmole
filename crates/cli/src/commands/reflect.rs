//! Reflection, review and citation. A reflection is private; a review is public prose plus a
//! rating; they never share a body.

use anyhow::{Result, bail};
use bookmole::{CreatedNote, Engine, EngineError, NoteKind, Reading};

use super::resolve_one;
use crate::{prompt, render};

pub struct ReflectOpts<'a> {
    pub book_selector: &'a str,
    /// Which reading, as `show` numbers them (1 = the first). Omitted, the
    /// current one.
    pub reading: Option<usize>,
    pub show: bool,
    pub no_edit: bool,
    pub rating: Option<f64>,
}

pub async fn reflect(engine: &Engine, opts: ReflectOpts<'_>) -> Result<()> {
    run(engine, opts, NoteKind::Reflection).await
}

pub async fn review(engine: &Engine, opts: ReflectOpts<'_>) -> Result<()> {
    run(engine, opts, NoteKind::Review).await
}

async fn run(engine: &Engine, opts: ReflectOpts<'_>, kind: NoteKind) -> Result<()> {
    let book = resolve_one(engine, opts.book_selector).await?;
    let book_id = book.id.expect("stored book has id");

    if opts.rating.is_some() && kind != NoteKind::Review {
        bail!("a rating belongs to a review — try `bookmole review {book_id} --rating …`");
    }

    let readings = engine.list_readings(book_id).await?;
    let reading_id = match opts.reading {
        Some(nth) => Some(nth_reading(&readings, nth)?),
        None => None,
    };

    // `--show` must not write: opening creates a reading when the book has none.
    if opts.show {
        return show_only(engine, &readings, reading_id, kind).await;
    }

    let note = match kind {
        NoteKind::Review => engine.open_review(book_id, reading_id).await?,
        _ => engine.open_reflection(book_id, reading_id).await?,
    };
    let record = engine.get_note(note.id).await?.expect("just opened");

    let readings = engine.list_readings(book_id).await?;
    if let Some(rid) = record.reading_id
        && let Some(i) = readings.iter().position(|r| r.id == rid)
    {
        println!("{}", book.display_title());
        println!(
            "  {}",
            render::reading_line(&readings[i], i + 1, readings.len())
        );
    }

    if !opts.no_edit {
        let body = engine.note_body(&record)?;
        let edited = prompt::edit_in_editor(&body)?;
        if edited.trim() != body.trim() {
            engine.update_note_body(&record, &edited).await?;
        }
    }

    if let Some(value) = opts.rating {
        engine.set_rating(note.id, value).await?;
        match engine.goodreads_rating(note.id).await {
            Ok(Some(g)) => println!("rated {value} → goodreads {g}"),
            Ok(None) => println!("rated {value}"),
            Err(EngineError::UnmappedRating { value, scale }) => {
                println!("rated {value}");
                println!(
                    "  no goodreads mapping yet: `bookmole rating map {value} <0-5 in quarters> --scale {scale}`"
                );
            }
            Err(e) => return Err(e.into()),
        }
    }

    let body = engine.note_body(&record)?;
    print_note(&note, &body);
    Ok(())
}

async fn show_only(
    engine: &Engine,
    readings: &[Reading],
    reading_id: Option<i64>,
    kind: NoteKind,
) -> Result<()> {
    let current = reading_id.or_else(|| {
        readings
            .iter()
            .find(|r| r.finished_at.is_none())
            .or(readings.last())
            .map(|r| r.id)
    });
    let existing = match current {
        Some(rid) => engine.note_for_reading(rid, kind.as_str()).await?,
        None => None,
    };
    let Some(record) = existing else {
        println!(
            "no {} yet — `bookmole {}` opens one",
            kind.as_str(),
            match kind {
                NoteKind::Review => "review <book>",
                _ => "reflect <book>",
            }
        );
        return Ok(());
    };
    let body = engine.note_body(&record)?;
    println!(
        "#{} “{}” -> {}",
        record.id,
        record.title,
        engine.note_path(&record)?.display()
    );
    print_body(&body);
    Ok(())
}

fn print_note(note: &CreatedNote, body: &str) {
    println!("#{} “{}” -> {}", note.id, note.title, note.file.display());
    print_body(body);
}

fn print_body(body: &str) {
    if body.trim().is_empty() {
        println!("  (empty — it accretes; come back to it)");
    } else {
        for line in body.trim_end().lines() {
            println!("  {line}");
        }
    }
}

fn nth_reading(readings: &[Reading], nth: usize) -> Result<i64> {
    if readings.is_empty() {
        bail!("this book has no readings yet — `bookmole progress <book> <page>` starts one");
    }
    match nth.checked_sub(1).and_then(|i| readings.get(i)) {
        Some(r) => Ok(r.id),
        None => bail!(
            "no reading {nth}: this book has {} (see `bookmole show`)",
            readings.len()
        ),
    }
}

/// Cite a highlight from a note, or with no highlight list what it cites. Cites by reference.
pub async fn cite(engine: &Engine, note_id: i64, highlight_id: Option<i64>) -> Result<()> {
    let Some(note) = engine.get_note(note_id).await? else {
        bail!("no note #{note_id} — try `bookmole notes`");
    };
    if let Some(hid) = highlight_id {
        engine.cite(note_id, hid).await?;
    }
    let cited = engine.citations_for(note_id).await?;
    if cited.is_empty() {
        println!("#{note_id} “{}” cites nothing yet", note.title);
        return Ok(());
    }
    println!("#{note_id} “{}” cites:", note.title);
    for h in cited {
        let page = h.page.map(|p| format!("p.{p} ")).unwrap_or_default();
        let chapter = h.chapter.map(|c| format!("[{c}] ")).unwrap_or_default();
        println!("  hl#{:<5} {page}{chapter}“{}”", h.id, h.text);
    }
    Ok(())
}
