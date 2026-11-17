use anyhow::Result;
use bookmole::{Engine, NewNoteInput, NoteKind, NoteScope};

use super::{resolve_note, resolve_one};
use crate::prompt;

pub struct NoteOpts<'a> {
    pub book_selector: Option<&'a str>,
    pub text: Option<String>,
    pub kind: &'a str,
    pub title: Option<String>,
    pub page: Option<i64>,
    pub no_page: bool,
    pub location: Option<String>,
    pub highlight: Option<i64>,
}

pub async fn create(engine: &Engine, opts: NoteOpts<'_>) -> Result<()> {
    let kind: NoteKind = opts.kind.parse()?;
    if kind.is_anchored() {
        let sel = opts.book_selector.unwrap_or("<book>");
        anyhow::bail!(
            "a {} is opened per reading: `bookmole {} {sel}`",
            kind.as_str(),
            kind.as_str().replace("reflection", "reflect")
        );
    }
    let book = match opts.book_selector {
        Some(sel) => Some(resolve_one(engine, sel).await?),
        None => None,
    };

    let page = match (opts.page, opts.no_page) {
        (Some(p), _) => Some(p),
        (None, true) => None,
        (None, false) => book.as_ref().and_then(|b| b.current_page),
    };

    let body = match opts.text {
        Some(t) if !t.trim().is_empty() => t,
        _ => prompt::edit_in_editor("")?,
    };
    if body.trim().is_empty() {
        println!("empty note, nothing saved.");
        return Ok(());
    }

    let created = engine
        .create_note(NewNoteInput {
            book_id: book.as_ref().and_then(|b| b.id),
            reading_id: None,
            highlight_id: opts.highlight,
            page,
            location: opts.location,
            kind,
            title: opts.title,
            body,
        })
        .await?;
    println!(
        "note #{} “{}” -> {}",
        created.id,
        created.title,
        created.file.display()
    );
    if !created.links.is_empty() {
        println!("links: {}", created.links.join(", "));
    }
    Ok(())
}

pub async fn list_or_search(
    engine: &Engine,
    book_selector: Option<&str>,
    query: Option<&str>,
) -> Result<()> {
    if let Some(q) = query {
        return super::find::find(
            engine,
            q,
            Some(bookmole::SearchSource::Note),
            super::find::DEFAULT_LIMIT,
        )
        .await;
    }

    let book = match book_selector {
        Some(sel) => Some(resolve_one(engine, sel).await?),
        None => None,
    };
    let notes = engine
        .list_notes(NoteScope::of_book(book.as_ref().and_then(|b| b.id)), None)
        .await?;
    if notes.is_empty() {
        println!("no notes yet — `bookmole note \"first thought\"`");
        return Ok(());
    }
    for n in notes {
        let kind = if n.kind == "note" {
            String::new()
        } else {
            format!(" [{}]", n.kind)
        };
        println!(
            "#{:<4} {}{kind}{}  ({})",
            n.id,
            n.title,
            anchor(&n),
            n.file_path
        );
    }
    Ok(())
}

/// What a note links to and what links back; dangling targets print as text, not dropped.
pub async fn links(engine: &Engine, selector: &str) -> Result<()> {
    super::catch_up(engine).await;
    let note = resolve_note(engine, selector).await?;
    println!("#{} “{}”  ({})", note.id, note.title, note.file_path);

    println!("links out:");
    let outgoing = engine.outgoing_links(note.id).await?;
    if outgoing.is_empty() {
        println!("  (none — a [[wikilink]] in the body makes one)");
    }
    for link in outgoing {
        match link.to {
            Some(t) => println!("  → #{:<4} “{}”", t.id, t.title),
            None => println!(
                "  → “{}”  (text — no note by that title yet)",
                link.target_title
            ),
        }
    }

    println!("links in:");
    let inbound = engine.backlinks(note.id).await?;
    if inbound.is_empty() {
        println!("  (nothing links here yet)");
    }
    for n in inbound {
        println!("  ← #{:<4} “{}”", n.id, n.title);
    }
    Ok(())
}

fn anchor(n: &bookmole::NoteRecord) -> String {
    let mut parts = Vec::new();
    if let Some(p) = n.page {
        parts.push(format!("p.{p}"));
    }
    if let Some(l) = &n.location {
        parts.push(l.clone());
    }
    if let Some(h) = n.highlight_id {
        parts.push(format!("↳hl#{h}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" @{}", parts.join(", "))
    }
}
