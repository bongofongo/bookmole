use std::path::Path;

use anyhow::{Result, bail};
use bookmole::{BookQuery, BookSort, Engine, NoteScope};

use super::resolve_one;
use crate::{prompt, render};

pub async fn add_isbn(engine: &Engine, isbn: &str, no_cover: bool) -> Result<()> {
    let outcome = engine.lookup_isbn_result(isbn).await?;
    for w in &outcome.warnings {
        eprintln!("warning: {w}");
    }
    let Some(found) = outcome.results.into_iter().next() else {
        bail!("no edition found for ISBN {isbn}");
    };
    let mut book = found.book;
    if !no_cover && book.cover_url.is_some() {
        match engine.download_cover(&mut book).await {
            Ok(Some(p)) => println!("cover -> {}", p.display()),
            Ok(None) => {}
            Err(e) => eprintln!("cover download failed: {e}"),
        }
    }
    let saved = engine
        .save_search_result_unrecorded(&book, &found.claims)
        .await?;
    println!("saved {}", render::book_line(&saved));
    Ok(())
}

pub async fn import_epub(engine: &Engine, path: &Path) -> Result<()> {
    let book = engine.import_epub(path).await?;
    if let Some(id) = book.id {
        engine.add_file_to_book(id, path).await?;
    }
    println!("imported {}", render::book_line(&book));
    if book.page_count.is_none() {
        println!("note: no page count found — set one with `progress` math in mind");
    }
    Ok(())
}

pub async fn list(engine: &Engine, limit: i64, sort: &str) -> Result<()> {
    let sort = match sort {
        "title" => BookSort::Title,
        "progress" => BookSort::Progress,
        "author" => BookSort::Author,
        "year" => BookSort::Year,
        "last-modified" | "last_modified" => BookSort::LastModified,
        other => {
            bail!("unknown sort '{other}' (last-modified | title | progress | author | year)")
        }
    };
    let books = engine.list_books(&BookQuery::new(limit, sort)).await?;
    if books.is_empty() {
        println!("library is empty — try `bookmole search` or `bookmole epub`");
        return Ok(());
    }
    for b in &books {
        println!("{}", render::book_line(b));
    }
    Ok(())
}

pub async fn show(engine: &Engine, selector: &str) -> Result<()> {
    let book = resolve_one(engine, selector).await?;
    print!("{}", render::book_details(&book));
    let notes = engine.list_notes(NoteScope::of_book(book.id), None).await?;
    if !notes.is_empty() {
        println!("  {:<14} {}", "notes", notes.len());
    }
    if let Some(id) = book.id {
        let highlights = engine.list_highlights(id).await?;
        if !highlights.is_empty() {
            println!("  {:<14} {}", "highlights", highlights.len());
        }
        let readings = engine.list_readings(id).await?;
        for (i, r) in readings.iter().enumerate() {
            println!("  {}", render::reading_line(r, i + 1, readings.len()));
        }
    }
    Ok(())
}

pub async fn toc(engine: &Engine, selector: &str) -> Result<()> {
    let book = resolve_one(engine, selector).await?;
    let id = book.id.expect("a stored book has an id");
    let title = book.display_title();

    let Some(toc) = engine.table_of_contents(id).await? else {
        println!("{title}: no epub here to read a chapter list from.");
        println!("    bookmole epub <path>");
        return Ok(());
    };
    if toc.entries.is_empty() {
        println!("{title}: this epub carries no table of contents.");
        println!("    read from {}", short_sha(&toc.sha256));
        return Ok(());
    }

    println!("{title} — {} chapters", toc.entries.len());
    for e in &toc.entries {
        println!("  {}{}", "  ".repeat(e.depth), e.label);
    }
    println!("    read from {}", short_sha(&toc.sha256));
    Ok(())
}

fn short_sha(sha256: &str) -> String {
    sha256.chars().take(12).collect()
}

pub async fn remove(engine: &Engine, selector: &str, yes: bool) -> Result<()> {
    let book = resolve_one(engine, selector).await?;
    let id = book.id.expect("stored book has id");
    if !yes && !prompt::confirm(&format!("remove {}?", render::book_line(&book)))? {
        println!("kept.");
        return Ok(());
    }
    engine.delete_book(id).await?;
    println!("removed {}", book.display_title());
    Ok(())
}

pub async fn progress(
    engine: &Engine,
    selector: &str,
    page: Option<i64>,
    finished: bool,
    reread: bool,
) -> Result<()> {
    let book = resolve_one(engine, selector).await?;
    let id = book.id.expect("stored book has id");
    if reread {
        if finished {
            bail!("--reread and --finished contradict each other");
        }
        engine.reread(id).await?;
    }
    let updated = engine
        .update_progress(id, page, finished.then_some(true))
        .await?;

    println!("{}", render::book_line(&updated));
    let readings = engine.list_readings(id).await?;
    let touched = engine.active_reading(id).await?;
    let touched = touched.as_ref().or(readings.last());
    if let Some(r) = touched
        && let Some(nth) = readings.iter().position(|x| x.id == r.id)
    {
        println!("  {}", render::reading_line(r, nth + 1, readings.len()));
    }
    if finished {
        println!(
            "\n🎉 finished {}! congratulations.",
            updated.display_title()
        );
        println!("your reflection is here when you want it:  bookmole reflect {id}");
    }
    Ok(())
}

pub async fn merge(
    engine: &Engine,
    src_selector: &str,
    dst_selector: &str,
    yes: bool,
) -> Result<()> {
    let src = resolve_one(engine, src_selector).await?;
    let dst = resolve_one(engine, dst_selector).await?;
    let (Some(src_id), Some(dst_id)) = (src.id, dst.id) else {
        bail!("both books must be saved before they can be merged");
    };
    if src_id == dst_id {
        println!("{} is already one book.", src.display_title());
        return Ok(());
    }

    if !yes
        && !prompt::confirm(&format!(
            "fold {} into {}? {} is deleted",
            render::book_line(&src),
            render::book_line(&dst),
            src.display_title()
        ))?
    {
        println!("cancelled.");
        return Ok(());
    }

    let r = engine.merge_books(src_id, dst_id).await?;
    println!("merged into {}", render::book_line(&dst));
    println!(
        "  {} highlights moved, {} dropped as duplicates",
        r.highlights_moved, r.highlights_dropped
    );
    println!(
        "  {} notes, {} flashcards moved ({} dropped), {} device links moved",
        r.notes_moved, r.flashcards_moved, r.flashcards_dropped, r.device_links_moved
    );
    println!("  {} readings moved", r.readings_moved);
    Ok(())
}

pub async fn highlights(engine: &Engine, selector: &str) -> Result<()> {
    let book = resolve_one(engine, selector).await?;
    let id = book.id.expect("stored book has id");
    let hs = engine.list_highlights(id).await?;
    if hs.is_empty() {
        println!(
            "no highlights for {} — try `bookmole ko import`",
            book.display_title()
        );
        return Ok(());
    }
    println!("{} — {} highlights\n", book.display_title(), hs.len());

    let readings = engine.list_readings(id).await?;
    if readings.len() < 2 {
        for h in &hs {
            print_highlight(h, "  ");
        }
        return Ok(());
    }

    for (i, r) in readings.iter().enumerate() {
        println!("  {}", render::reading_line(r, i + 1, readings.len()));
        let mine: Vec<_> = hs.iter().filter(|h| h.reading_id == Some(r.id)).collect();
        if mine.is_empty() {
            println!("    (none)");
        }
        for h in mine {
            print_highlight(h, "    ");
        }
        println!();
    }

    // `reading_id` is NULL when no reading's window holds the capture date; those are listed last.
    let unplaced: Vec<_> = hs.iter().filter(|h| h.reading_id.is_none()).collect();
    if !unplaced.is_empty() {
        println!("  not placed in a reading ({})", unplaced.len());
        for h in unplaced {
            print_highlight(h, "    ");
        }
    }
    Ok(())
}

fn print_highlight(h: &bookmole::Highlight, indent: &str) {
    let page = h.page.map(|p| format!("p.{p} ")).unwrap_or_default();
    let chapter = h
        .chapter
        .as_deref()
        .map(|c| format!("[{c}] "))
        .unwrap_or_default();
    println!("{indent}{page}{chapter}“{}”", h.text);
    if let Some(note) = &h.origin_note {
        println!("{indent}    ↳ {note}");
    }
    if let Some(annotation) = &h.annotation {
        println!("{indent}    » {annotation}");
    }
}
