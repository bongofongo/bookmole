//! One search over notes and highlights, returned as one list.
//! Order is by within-source rank, never raw bm25, which is not comparable across the two indexes.
//! A `book_id` scope is applied in both queries before the `LIMIT`; filtering hits afterwards is
//! wrong.

use sqlx::Row;

use super::highlights::{HIGHLIGHT_COLUMNS, row_to_highlight};
use super::notes::{NOTE_COLUMNS, qualified, row_to_note};
use super::{Highlight, NoteRecord, Storage};
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSource {
    Note,
    Highlight,
}

#[derive(Debug, Clone)]
pub enum SearchHit {
    Note {
        note: NoteRecord,
        snippet: String,
    },
    Highlight {
        highlight: Highlight,
        snippet: String,
    },
}

impl SearchHit {
    pub fn source(&self) -> SearchSource {
        match self {
            SearchHit::Note { .. } => SearchSource::Note,
            SearchHit::Highlight { .. } => SearchSource::Highlight,
        }
    }

    pub fn snippet(&self) -> &str {
        match self {
            SearchHit::Note { snippet, .. } | SearchHit::Highlight { snippet, .. } => snippet,
        }
    }

    pub fn as_note(&self) -> Option<&NoteRecord> {
        match self {
            SearchHit::Note { note, .. } => Some(note),
            _ => None,
        }
    }

    pub fn as_highlight(&self) -> Option<&Highlight> {
        match self {
            SearchHit::Highlight { highlight, .. } => Some(highlight),
            _ => None,
        }
    }

    pub fn book_id(&self) -> Option<i64> {
        match self {
            SearchHit::Note { note, .. } => note.book_id,
            SearchHit::Highlight { highlight, .. } => Some(highlight.book_id),
        }
    }

    fn stored_at(&self) -> Option<i64> {
        match self {
            SearchHit::Note { note, .. } => note.created_at.map(|t| t.unix_timestamp()),
            SearchHit::Highlight { highlight, .. } => Some(highlight.created_at),
        }
    }

    fn id(&self) -> i64 {
        match self {
            SearchHit::Note { note, .. } => note.id,
            SearchHit::Highlight { highlight, .. } => highlight.id,
        }
    }
}

/// Shared by both queries so their snippets are shaped identically.
const SNIPPET_TOKENS: i64 = 12;

/// Every token becomes a quoted phrase, because fts5 reads `-`, `*`, `:`, `'` and `NEAR` as syntax.
/// Only a trailing `*` (prefix search) survives; tokens are ANDed.
pub(super) fn fts_query(raw: &str) -> Option<String> {
    let phrases = fts_phrases(raw);
    (!phrases.is_empty()).then(|| phrases.join(" "))
}

fn fts_any(raw: &str) -> Option<String> {
    let phrases = fts_phrases(raw);
    (!phrases.is_empty()).then(|| phrases.join(" OR "))
}

fn fts_phrases(raw: &str) -> Vec<String> {
    let mut phrases: Vec<String> = Vec::new();
    for token in raw.split_whitespace() {
        let (body, prefix) = match token.strip_suffix('*') {
            Some(stem) => (stem, true),
            None => (token, false),
        };
        if body.is_empty() {
            // A lone `*` is punctuation, not a query for everything.
            continue;
        }
        let quoted = body.replace('"', "\"\"");
        phrases.push(if prefix {
            format!("\"{quoted}\"*")
        } else {
            format!("\"{quoted}\"")
        });
    }
    phrases
}

fn interleave(notes: Vec<SearchHit>, highlights: Vec<SearchHit>, limit: usize) -> Vec<SearchHit> {
    let mut ranked: Vec<(usize, SearchHit)> = Vec::with_capacity(notes.len() + highlights.len());
    for (position, hit) in notes.into_iter().enumerate() {
        ranked.push((position, hit));
    }
    for (position, hit) in highlights.into_iter().enumerate() {
        ranked.push((position, hit));
    }
    // A hit with no decodable timestamp sorts last: `None` is not the beginning of time.
    ranked.sort_by_key(|(position, hit)| {
        (
            *position,
            hit.stored_at().is_none(),
            std::cmp::Reverse(hit.stored_at().unwrap_or(0)),
            match hit.source() {
                SearchSource::Note => 0u8,
                SearchSource::Highlight => 1,
            },
            hit.id(),
        )
    });
    ranked.into_iter().take(limit).map(|(_, hit)| hit).collect()
}

impl Storage {
    /// `book_id` is applied inside both queries, before `limit`; filtering the result afterwards
    /// would cut the global list first and can return nothing.
    pub async fn search_marks(
        &self,
        query: &str,
        source: Option<SearchSource>,
        book_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<SearchHit>> {
        let Some(prepared) = fts_query(query) else {
            return Ok(Vec::new());
        };
        // Search queries are private reading: `trace!` is the ceiling.
        tracing::trace!(query = %prepared, "searching notes and highlights");
        let want = limit.max(0);

        let notes = match source {
            Some(SearchSource::Highlight) => Vec::new(),
            _ => self.search_note_index(&prepared, book_id, want).await?,
        };
        let highlights = match source {
            Some(SearchSource::Note) => Vec::new(),
            _ => {
                self.search_highlight_index(&prepared, book_id, want)
                    .await?
            }
        };
        let hits = interleave(notes, highlights, want as usize);
        tracing::debug!(hits = hits.len(), "search answered");
        Ok(hits)
    }

    async fn search_note_index(
        &self,
        prepared: &str,
        book_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<SearchHit>> {
        let columns = qualified(NOTE_COLUMNS, "n");
        let scope = book_scope("n", book_id);
        let sql = format!(
            "SELECT {columns},
                    snippet(notes_fts, -1, '>>', '<<', '…', {SNIPPET_TOKENS}) AS snip
             FROM notes_fts JOIN notes n ON n.id = notes_fts.rowid
             WHERE notes_fts MATCH ?{scope} ORDER BY rank LIMIT ?"
        );
        let mut q = sqlx::query(&sql).bind(prepared);
        if let Some(id) = book_id {
            q = q.bind(id);
        }
        let rows = q.bind(limit).fetch_all(self.pool()).await?;
        rows.iter()
            .map(|r| {
                Ok(SearchHit::Note {
                    note: row_to_note(r)?,
                    snippet: r.try_get("snip")?,
                })
            })
            .collect()
    }

    async fn search_highlight_index(
        &self,
        prepared: &str,
        book_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<SearchHit>> {
        let columns = qualified(HIGHLIGHT_COLUMNS, "h");
        let scope = book_scope("h", book_id);
        let sql = format!(
            "SELECT {columns},
                    snippet(highlights_fts, -1, '>>', '<<', '…', {SNIPPET_TOKENS}) AS snip
             FROM highlights_fts JOIN highlights h ON h.id = highlights_fts.rowid
             WHERE highlights_fts MATCH ?{scope} ORDER BY rank LIMIT ?"
        );
        let mut q = sqlx::query(&sql).bind(prepared);
        if let Some(id) = book_id {
            q = q.bind(id);
        }
        let rows = q.bind(limit).fetch_all(self.pool()).await?;
        rows.iter()
            .map(|r| {
                Ok(SearchHit::Highlight {
                    highlight: row_to_highlight(r)?,
                    snippet: r.try_get("snip")?,
                })
            })
            .collect()
    }
}

impl Storage {
    /// Keys of `texts` matching `query` by `notes_fts`'s own rules, ascending; when no text holds
    /// every term, those holding any term.
    pub(crate) async fn matching_texts(
        &self,
        query: &str,
        texts: &[(i64, &str)],
    ) -> Result<Vec<i64>> {
        let (Some(all), Some(any)) = (fts_query(query), fts_any(query)) else {
            return Ok(Vec::new());
        };
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "CREATE VIRTUAL TABLE IF NOT EXISTS temp.text_probe
             USING fts5(body, tokenize='porter unicode61')",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM temp.text_probe")
            .execute(&mut *tx)
            .await?;
        for (key, text) in texts {
            sqlx::query("INSERT INTO temp.text_probe (rowid, body) VALUES (?, ?)")
                .bind(key)
                .bind(text)
                .execute(&mut *tx)
                .await?;
        }
        let mut keys = Vec::new();
        for prepared in [all, any] {
            keys = sqlx::query_scalar::<_, i64>(
                "SELECT rowid FROM temp.text_probe WHERE text_probe MATCH ? ORDER BY rowid",
            )
            .bind(&prepared)
            .fetch_all(&mut *tx)
            .await?;
            if !keys.is_empty() {
                break;
            }
        }
        tx.rollback().await?;
        Ok(keys)
    }
}

fn book_scope(alias: &str, book_id: Option<i64>) -> String {
    match book_id {
        Some(_) => format!(" AND {alias}.book_id = ?"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::Book;
    use crate::storage::{NewHighlight, NewNoteMeta};

    #[test]
    fn punctuation_a_reader_types_is_not_a_syntax_error() {
        assert_eq!(fts_query("don't"), Some(r#""don't""#.into()));
        assert_eq!(fts_query("C++"), Some(r#""C++""#.into()));
        assert_eq!(fts_query("grief -- and after"), {
            Some(r#""grief" "--" "and" "after""#.into())
        });
    }

    #[test]
    fn a_quote_cannot_escape_its_own_phrase() {
        assert_eq!(
            fts_query(r#"say "hello""#),
            Some(r#""say" """hello""""#.into())
        );
    }

    #[test]
    fn an_empty_query_is_not_asking() {
        assert_eq!(fts_query(""), None);
        assert_eq!(fts_query("   \t\n "), None);
        assert_eq!(fts_query("*"), None, "a lone star is punctuation");
    }

    #[test]
    fn a_trailing_star_stays_a_prefix_search() {
        assert_eq!(fts_query("resil*"), Some(r#""resil"*"#.into()));
        assert_eq!(fts_query("a resil*"), Some(r#""a" "resil"*"#.into()));
    }

    fn note_hit(id: i64, at: i64) -> SearchHit {
        SearchHit::Note {
            note: NoteRecord {
                id,
                book_id: None,
                reading_id: None,
                highlight_id: None,
                page: None,
                location: None,
                file_path: format!("unsorted/{id}.md"),
                title: format!("note {id}"),
                kind: "note".into(),
                created_at: time::OffsetDateTime::from_unix_timestamp(at).ok(),
                last_modified: None,
            },
            snippet: String::new(),
        }
    }

    fn highlight_hit(id: i64, at: i64) -> SearchHit {
        SearchHit::Highlight {
            highlight: Highlight {
                id,
                book_id: 1,
                text: format!("passage {id}"),
                chapter: None,
                page: None,
                origin_note: None,
                annotation: None,
                ko_datetime: None,
                reading_id: None,
                source: "koreader".into(),
                origin: crate::HighlightOrigin::Koreader,
                copy_id: None,
                origin_id: None,
                pos_kind: None,
                text_unavailable: false,
                created_at: at,
            },
            snippet: String::new(),
        }
    }

    #[test]
    fn the_two_sources_interleave_by_their_own_rank() {
        let notes = vec![note_hit(1, 100), note_hit(2, 100), note_hit(3, 100)];
        let highlights = vec![highlight_hit(10, 200), highlight_hit(11, 200)];
        let merged = interleave(notes, highlights, 10);
        assert_eq!(
            merged.iter().map(|h| h.id()).collect::<Vec<_>>(),
            vec![10, 1, 11, 2, 3],
            "position 0 holds both firsts, and the newer one leads"
        );
    }

    #[test]
    fn a_source_is_never_reordered_against_itself() {
        let notes = (0..6).map(|i| note_hit(i, 500 - i)).collect::<Vec<_>>();
        let highlights = (0..6)
            .map(|i| highlight_hit(100 + i, i))
            .collect::<Vec<_>>();
        let merged = interleave(notes, highlights, 20);
        let note_order: Vec<i64> = merged
            .iter()
            .filter_map(|h| h.as_note())
            .map(|n| n.id)
            .collect();
        assert_eq!(note_order, vec![0, 1, 2, 3, 4, 5]);
        let hl_order: Vec<i64> = merged
            .iter()
            .filter_map(|h| h.as_highlight())
            .map(|h| h.id)
            .collect();
        assert_eq!(hl_order, vec![100, 101, 102, 103, 104, 105]);
    }

    #[test]
    fn one_source_alone_is_its_own_ranked_list() {
        let notes = vec![note_hit(1, 3), note_hit(2, 2), note_hit(3, 1)];
        let merged = interleave(notes, Vec::new(), 10);
        assert_eq!(
            merged.iter().map(|h| h.id()).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn a_hit_with_no_timestamp_sorts_last_at_its_position() {
        let undated = SearchHit::Note {
            note: NoteRecord {
                created_at: None,
                ..match note_hit(1, 0) {
                    SearchHit::Note { note, .. } => note,
                    _ => unreachable!(),
                }
            },
            snippet: String::new(),
        };
        let merged = interleave(vec![undated], vec![highlight_hit(10, 1)], 10);
        assert_eq!(
            merged.iter().map(|h| h.id()).collect::<Vec<_>>(),
            vec![10, 1]
        );
    }

    async fn seeded() -> (Storage, i64) {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let book = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        (s, book)
    }

    fn hl(text: &str) -> NewHighlight {
        NewHighlight {
            text: text.into(),
            chapter: Some("Ch 1".into()),
            page: Some(42),
            pos0: Some("/body/DocFragment[8]/p[3]/text().0".into()),
            pos1: None,
            ko_datetime: Some("2026-01-01 10:00:00".into()),
            ko_datetime_updated: None,
            color: None,
            note: None,
            source: "koreader".into(),
        }
    }

    #[tokio::test]
    async fn an_imported_highlight_is_findable() {
        let (s, book) = seeded().await;
        s.insert_highlight(book, &hl("we are all of us born in moral stupidity"))
            .await
            .unwrap();

        let hits = s.search_marks("moral", None, None, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].as_highlight().unwrap().text,
            "we are all of us born in moral stupidity"
        );
        assert!(
            hits[0].snippet().contains(">>moral<<"),
            "{}",
            hits[0].snippet()
        );
        assert_eq!(hits[0].book_id(), Some(book));
    }

    #[tokio::test]
    async fn a_note_and_a_highlight_share_one_list() {
        let (s, book) = seeded().await;
        s.insert_highlight(book, &hl("the antechamber of grief"))
            .await
            .unwrap();
        s.insert_note(
            NewNoteMeta {
                book_id: Some(book),
                reading_id: None,
                highlight_id: None,
                page: None,
                location: None,
                file_path: "middlemarch/a.md",
                title: "On grief",
                kind: "note",
            },
            "What grief does to a family over four generations.",
            &[],
        )
        .await
        .unwrap();

        let hits = s.search_marks("grief", None, None, 10).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|h| h.as_note().is_some()));
        assert!(hits.iter().any(|h| h.as_highlight().is_some()));
        for h in &hits {
            assert!(h.snippet().contains(">>"), "every hit carries a snippet");
        }
    }

    #[tokio::test]
    async fn a_source_filter_narrows_the_list_and_nothing_else() {
        let (s, book) = seeded().await;
        s.insert_highlight(book, &hl("the antechamber of grief"))
            .await
            .unwrap();
        s.insert_note(
            NewNoteMeta {
                book_id: Some(book),
                reading_id: None,
                highlight_id: None,
                page: None,
                location: None,
                file_path: "middlemarch/a.md",
                title: "On grief",
                kind: "note",
            },
            "What grief does to a family.",
            &[],
        )
        .await
        .unwrap();

        let notes = s
            .search_marks("grief", Some(SearchSource::Note), None, 10)
            .await
            .unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].as_note().is_some());

        let highlights = s
            .search_marks("grief", Some(SearchSource::Highlight), None, 10)
            .await
            .unwrap();
        assert_eq!(highlights.len(), 1);
        assert!(highlights[0].as_highlight().is_some());
    }

    #[tokio::test]
    async fn an_empty_query_answers_nothing_rather_than_failing() {
        let (s, book) = seeded().await;
        s.insert_highlight(book, &hl("a passage worth keeping"))
            .await
            .unwrap();
        for q in ["", "   ", "\t"] {
            assert!(s.search_marks(q, None, None, 10).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn an_apostrophe_is_searched_for_rather_than_parsed() {
        let (s, book) = seeded().await;
        s.insert_highlight(book, &hl("she couldn't say it aloud"))
            .await
            .unwrap();
        let hits = s.search_marks("couldn't", None, None, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(
            s.search_marks("C++", None, None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn no_hits_is_an_answer() {
        let (s, book) = seeded().await;
        s.insert_highlight(book, &hl("a passage worth keeping"))
            .await
            .unwrap();
        assert!(
            s.search_marks("thermodynamics", None, None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn every_write_path_moves_the_index() {
        let (s, book) = seeded().await;
        let found = |s: Storage, q: &'static str| async move {
            s.search_marks(q, Some(SearchSource::Highlight), None, 10)
                .await
                .unwrap()
                .len()
        };

        let h = hl("born in moral stupidity");
        let id = s.insert_highlight(book, &h).await.unwrap().unwrap();
        assert_eq!(found(s.clone(), "moral").await, 1);

        let edited = NewHighlight {
            note: Some("they mean the provinces".into()),
            ..h.clone()
        };
        assert!(s.refresh_device_fields(book, &edited).await.unwrap());
        assert_eq!(found(s.clone(), "provinces").await, 1);

        assert!(s.refresh_device_fields(book, &h).await.unwrap());
        assert_eq!(found(s.clone(), "provinces").await, 0);

        s.set_annotation(id, Some("the whole book in one line"))
            .await
            .unwrap();
        assert_eq!(found(s.clone(), "whole").await, 1);
        assert_eq!(
            found(s.clone(), "moral").await,
            1,
            "indexing the annotation must not drop the passage"
        );

        s.delete_book(book).await.unwrap();
        assert_eq!(found(s.clone(), "moral").await, 0);
    }

    #[tokio::test]
    async fn a_merge_leaves_the_index_agreeing_with_the_table() {
        let (s, dst) = seeded().await;
        let src = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch (duplicate)".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();

        let shared = hl("born in moral stupidity");
        let only_src = NewHighlight {
            text: "a passage the duplicate alone holds".into(),
            ko_datetime: Some("2026-02-02 10:00:00".into()),
            ..shared.clone()
        };
        s.insert_highlight(dst, &shared).await.unwrap();
        s.insert_highlight(src, &shared).await.unwrap();
        s.insert_highlight(src, &only_src).await.unwrap();

        s.merge_books(src, dst).await.unwrap();

        let moved = s
            .search_marks("duplicate alone", Some(SearchSource::Highlight), None, 10)
            .await
            .unwrap();
        assert_eq!(moved.len(), 1, "a moved highlight stays findable");
        assert_eq!(moved[0].as_highlight().unwrap().book_id, dst);

        let deduped = s
            .search_marks("moral", Some(SearchSource::Highlight), None, 10)
            .await
            .unwrap();
        assert_eq!(
            deduped.len(),
            1,
            "the dropped copy left the index with the row"
        );
    }

    fn book(title: &str) -> Book {
        Book {
            title: Some(title.into()),
            ..Default::default()
        }
    }

    fn ident(h: &SearchHit) -> (u8, i64) {
        (
            match h.source() {
                SearchSource::Note => 0,
                SearchSource::Highlight => 1,
            },
            h.id(),
        )
    }

    /// Decoys are one-word passages so bm25 ranks them above the target's longer one.
    #[tokio::test]
    async fn a_scoped_search_finds_what_a_filter_above_the_seam_would_have_truncated() {
        const LIMIT: i64 = 5;
        const DECOYS: usize = 30;
        let (s, _) = seeded().await;

        for i in 0..DECOYS {
            let other = s
                .upsert_book(&book(&format!("Decoy {i}")), None)
                .await
                .unwrap();
            s.insert_highlight(other, &hl("grief")).await.unwrap();
        }
        let wanted = s
            .upsert_book(&book("The Remains of the Day"), None)
            .await
            .unwrap();
        s.insert_highlight(
            wanted,
            &hl("the antechamber of a feeling she would only much later learn to call grief"),
        )
        .await
        .unwrap();

        let unbounded = s.search_marks("grief", None, None, 1000).await.unwrap();
        assert_eq!(unbounded.len(), DECOYS + 1);
        let buried = unbounded
            .iter()
            .position(|h| h.book_id() == Some(wanted))
            .expect("the wanted book matches the query at all");
        assert!(
            buried >= LIMIT as usize,
            "the fixture must rank the wanted book below the limit; it is at {buried}"
        );

        let truncated = s.search_marks("grief", None, None, LIMIT).await.unwrap();
        assert_eq!(truncated.len(), LIMIT as usize);
        assert!(
            !truncated.iter().any(|h| h.book_id() == Some(wanted)),
            "nothing to filter — this is the empty book view the scope prevents"
        );

        let scoped = s
            .search_marks("grief", None, Some(wanted), LIMIT)
            .await
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].book_id(), Some(wanted));
        assert!(scoped[0].snippet().contains(">>grief<<"));
    }

    /// Interleaving order is deliberately not asserted: filtering compacts within-source positions.
    #[tokio::test]
    async fn scoping_restricts_membership_and_never_a_source_s_own_order() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let mut books = Vec::new();
        for i in 0..4usize {
            let b = s
                .upsert_book(&book(&format!("Book {i}")), None)
                .await
                .unwrap();
            // Varying counts and lengths, so the two ranked lists genuinely
            // interleave rather than running parallel.
            for j in 0..=i {
                s.insert_highlight(
                    b,
                    &NewHighlight {
                        text: format!("{}grief and the shape of it", "a passage on ".repeat(j + 1)),
                        ko_datetime: Some(format!("2026-01-0{} 10:00:00", j + 1)),
                        ..hl("seed")
                    },
                )
                .await
                .unwrap();
                let path = format!("b{i}/n{j}.md");
                let title = format!("note {i}-{j}");
                let body = format!("{}on grief", "thinking ".repeat(j + 1));
                s.insert_note(
                    NewNoteMeta {
                        book_id: Some(b),
                        reading_id: None,
                        highlight_id: None,
                        page: None,
                        location: None,
                        file_path: &path,
                        title: &title,
                        kind: "note",
                    },
                    &body,
                    &[],
                )
                .await
                .unwrap();
            }
            books.push(b);
        }
        s.insert_note(
            NewNoteMeta {
                book_id: None,
                reading_id: None,
                highlight_id: None,
                page: None,
                location: None,
                file_path: "unsorted/free.md",
                title: "a free thought",
                kind: "note",
            },
            "grief, unattached to any book",
            &[],
        )
        .await
        .unwrap();

        let all = s.search_marks("grief", None, None, 1000).await.unwrap();
        assert!(
            all.iter().any(|h| h.book_id().is_none()),
            "the unanchored note is in the library-wide answer"
        );

        for &b in &books {
            let scoped = s.search_marks("grief", None, Some(b), 1000).await.unwrap();
            assert!(!scoped.is_empty());

            let want: std::collections::BTreeSet<_> = all
                .iter()
                .filter(|h| h.book_id() == Some(b))
                .map(ident)
                .collect();
            let got: std::collections::BTreeSet<_> = scoped.iter().map(ident).collect();
            assert_eq!(got, want, "membership, for book {b}");

            for src in [SearchSource::Note, SearchSource::Highlight] {
                let unscoped_order: Vec<i64> = all
                    .iter()
                    .filter(|h| h.source() == src && h.book_id() == Some(b))
                    .map(|h| h.id())
                    .collect();
                let scoped_order: Vec<i64> = scoped
                    .iter()
                    .filter(|h| h.source() == src)
                    .map(|h| h.id())
                    .collect();
                assert_eq!(
                    scoped_order, unscoped_order,
                    "a WHERE may not reorder {src:?}'s own rank, for book {b}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_two_narrowings_compose() {
        let (s, middlemarch) = seeded().await;
        let other = s.upsert_book(&book("Beloved"), None).await.unwrap();
        for b in [middlemarch, other] {
            s.insert_highlight(b, &hl("the antechamber of grief"))
                .await
                .unwrap();
            let path = format!("b{b}/a.md");
            s.insert_note(
                NewNoteMeta {
                    book_id: Some(b),
                    reading_id: None,
                    highlight_id: None,
                    page: None,
                    location: None,
                    file_path: &path,
                    title: "On grief",
                    kind: "note",
                },
                "What grief does to a family.",
                &[],
            )
            .await
            .unwrap();
        }

        assert_eq!(
            s.search_marks("grief", None, None, 10).await.unwrap().len(),
            4
        );
        assert_eq!(
            s.search_marks("grief", None, Some(middlemarch), 10)
                .await
                .unwrap()
                .len(),
            2
        );
        let one = s
            .search_marks(
                "grief",
                Some(SearchSource::Highlight),
                Some(middlemarch),
                10,
            )
            .await
            .unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].as_highlight().unwrap().book_id, middlemarch);
    }

    #[tokio::test]
    async fn scoping_to_a_book_with_no_marks_is_an_answer() {
        let (s, middlemarch) = seeded().await;
        s.insert_highlight(middlemarch, &hl("the antechamber of grief"))
            .await
            .unwrap();
        let empty = s.upsert_book(&book("Beloved"), None).await.unwrap();
        assert!(
            s.search_marks("grief", None, Some(empty), 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            s.search_marks("grief", None, Some(empty + 1000), 10)
                .await
                .unwrap()
                .is_empty(),
            "a book id nothing holds is empty, not everything"
        );
    }

    #[tokio::test]
    async fn the_probe_matches_by_the_index_s_own_rules() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let texts = [
            (1, "the first part, about nothing"),
            (2, "we walked to the harbour"),
            (3, "she couldn't say it, walking home"),
        ];
        assert_eq!(s.matching_texts("walk", &texts).await.unwrap(), vec![2, 3]);
        assert_eq!(s.matching_texts("couldn't", &texts).await.unwrap(), vec![3]);
        assert_eq!(
            s.matching_texts("harbour walked", &texts).await.unwrap(),
            vec![2],
            "a part holding every term is the answer"
        );
        assert!(s.matching_texts("", &texts).await.unwrap().is_empty());
        assert!(s.matching_texts("absent", &texts).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn terms_split_across_parts_answer_the_parts_holding_any() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let texts = [(1, "the harbour"), (2, "nothing here"), (3, "a lighthouse")];
        assert_eq!(
            s.matching_texts("harbour lighthouse", &texts)
                .await
                .unwrap(),
            vec![1, 3]
        );
    }

    #[tokio::test]
    async fn the_probe_leaves_nothing_behind() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        s.matching_texts("harbour", &[(1, "the harbour")])
            .await
            .unwrap();
        assert!(
            s.matching_texts("harbour", &[(2, "a lighthouse")])
                .await
                .unwrap()
                .is_empty()
        );
        let left: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_temp_master WHERE name LIKE 'text_probe%'",
        )
        .fetch_one(s.pool())
        .await
        .unwrap();
        assert_eq!(left, 0);
    }
}
