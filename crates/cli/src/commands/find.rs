//! `notes --search` calls the same method, so matching and ordering agree.

use std::collections::HashMap;

use anyhow::Result;
use bookmole::{Engine, SearchHit, SearchSource};

pub const DEFAULT_LIMIT: i64 = 25;

pub async fn find(
    engine: &Engine,
    query: &str,
    source: Option<SearchSource>,
    limit: i64,
) -> Result<()> {
    super::catch_up(engine).await;

    let hits = engine.search_marks(query, source, None, limit).await?;
    if hits.is_empty() {
        println!("{}", nothing(query, source));
        return Ok(());
    }

    let mut titles: HashMap<i64, String> = HashMap::new();
    for id in hits.iter().filter_map(|h| h.book_id()) {
        if let std::collections::hash_map::Entry::Vacant(slot) = titles.entry(id)
            && let Some(book) = engine.get_book(id).await?
        {
            slot.insert(book.title.unwrap_or_else(|| format!("book #{id}")));
        }
    }

    for hit in &hits {
        let book = hit.book_id().and_then(|id| titles.get(&id)).cloned();
        println!("{}", headline(hit, book.as_deref()));
        println!("      {}", hit.snippet());
    }
    Ok(())
}

/// Pure so the wording is testable; never prints `0 results`, and an empty query is not a failed
/// search.
fn nothing(query: &str, source: Option<SearchSource>) -> String {
    if query.trim().is_empty() {
        return "nothing to search for — try `bookmole find grief`".to_string();
    }
    let what = match source {
        None => "notes or highlights",
        Some(SearchSource::Note) => "notes",
        Some(SearchSource::Highlight) => "highlights",
    };
    format!("no {what} match '{query}'")
}

fn headline(hit: &SearchHit, book: Option<&str>) -> String {
    match hit {
        SearchHit::Note { note, .. } => {
            let where_from = match book {
                Some(title) => format!("{title} — {}", note.file_path),
                None => note.file_path.clone(),
            };
            format!("note      #{:<5} {}  ({where_from})", note.id, note.title)
        }
        SearchHit::Highlight { highlight, .. } => {
            let mut where_from = book.unwrap_or("").to_string();
            if let Some(page) = highlight.page {
                if !where_from.is_empty() {
                    where_from.push_str(" — ");
                }
                where_from.push_str(&format!("p.{page}"));
            }
            format!(
                "highlight #{:<5} {}",
                highlight.id,
                if where_from.is_empty() {
                    String::new()
                } else {
                    format!("({where_from})")
                }
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bookmole::{Highlight, NoteRecord};

    fn note(id: i64, book_id: Option<i64>) -> SearchHit {
        SearchHit::Note {
            note: NoteRecord {
                id,
                book_id,
                reading_id: None,
                highlight_id: None,
                page: None,
                location: None,
                file_path: "middlemarch/1-on-grief.md".into(),
                title: "On grief".into(),
                kind: "note".into(),
                created_at: None,
                last_modified: None,
            },
            snippet: "what >>grief<< does".into(),
        }
    }

    fn highlight(id: i64, page: Option<i64>) -> SearchHit {
        SearchHit::Highlight {
            highlight: Highlight {
                id,
                book_id: 1,
                text: "born in moral stupidity".into(),
                chapter: None,
                page,
                origin_note: None,
                annotation: None,
                ko_datetime: None,
                reading_id: None,
                source: "koreader".into(),
                origin: bookmole::HighlightOrigin::Koreader,
                copy_id: None,
                origin_id: None,
                pos_kind: None,
                text_unavailable: false,
                created_at: 0,
            },
            snippet: "born in >>moral<< stupidity".into(),
        }
    }

    #[test]
    fn nothing_found_is_never_reported_as_zero() {
        let s = nothing("thermodynamics", None);
        assert!(!s.contains('0'), "absence is not zero: {s}");
        assert!(
            s.contains("no notes or highlights match 'thermodynamics'"),
            "{s}"
        );
        assert!(
            nothing("x", Some(SearchSource::Highlight)).contains("no highlights match"),
            "the narrowing has to be named, or an empty answer looks like an \
             empty library"
        );
    }

    #[test]
    fn an_empty_query_is_not_a_failed_search() {
        let s = nothing("   ", None);
        assert!(s.contains("nothing to search for"), "{s}");
        assert!(!s.contains("match"), "{s}");
    }

    #[test]
    fn every_hit_says_what_kind_of_thing_it_is() {
        assert!(headline(&note(12, Some(1)), Some("Middlemarch")).starts_with("note "));
        assert!(headline(&highlight(48, Some(42)), Some("Middlemarch")).starts_with("highlight "));
    }

    #[test]
    fn an_unanchored_note_names_no_book() {
        let line = headline(&note(12, None), None);
        assert!(line.contains("On grief"), "{line}");
        assert!(!line.contains("—"), "{line}");
        assert!(!line.contains("None"), "{line}");
    }

    #[test]
    fn a_highlight_without_a_page_still_names_its_book() {
        let line = headline(&highlight(48, None), Some("Middlemarch"));
        assert!(line.contains("Middlemarch"), "{line}");
        assert!(!line.contains("p."), "{line}");
    }
}
