use std::collections::HashMap;

use crate::Engine;
use crate::book::{Book, ReadingState};
use crate::error::Result;
use crate::history::{HistoryRead, summarise_reads};
use crate::storage::{BookSort, now_unix};

#[derive(Debug, Clone)]
pub struct LibraryRow {
    pub book: Book,
    /// 0-based position in the engine's Title order (leading article dropped).
    /// A position, not a key: a client re-collating titles would disagree with SQLite's `NOCASE`.
    pub title_order: i64,
    /// The same for Author order: first author's surname, then the whole name; authorless last.
    pub author_order: i64,
    /// The current read (the open one, else the most recent), chosen by the rule `BOOK_FROM` uses;
    /// `None` for a book never read.
    pub latest: Option<HistoryRead>,
    pub passages: i64,
    pub notes: i64,
}

/// The filters' counts; they partition `all`: each book is in exactly one by its latest read's
/// state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LibraryCounts {
    pub all: i64,
    pub reading: i64,
    pub read: i64,
    pub put_down: i64,
    /// A latest read whose status this build does not model, shown verbatim on the row.
    pub other: i64,
    pub no_reading: i64,
}

#[derive(Debug, Clone)]
pub struct LibraryTable {
    pub rows: Vec<LibraryRow>,
    pub counts: LibraryCounts,
    pub total_pages: i64,
}

/// The current read of each book: open first, then latest start (`COALESCE(started_at,
/// created_at)`), then higher id; `BOOK_FROM`'s rule over rows already in hand.
fn current_reads(reads: Vec<HistoryRead>) -> HashMap<i64, HistoryRead> {
    let key = |r: &HistoryRead| {
        (
            r.reading.finished_at.is_none(),
            r.reading.started_at.unwrap_or(r.reading.created_at),
            r.reading.id,
        )
    };
    let mut out: HashMap<i64, HistoryRead> = HashMap::new();
    for r in reads {
        match out.get(&r.reading.book_id) {
            Some(cur) if key(cur) >= key(&r) => {}
            _ => {
                out.insert(r.reading.book_id, r);
            }
        }
    }
    out
}

impl Engine {
    pub async fn library_table(&self) -> Result<LibraryTable> {
        let mut facts = self.history_facts(false).await?;
        let in_library: std::collections::HashSet<i64> =
            self.storage.in_library_ids().await?.into_iter().collect();
        facts.books.retain(|id, _| in_library.contains(id));
        let mut latest = current_reads(summarise_reads(&facts, now_unix()));
        let title = self.sort_positions(BookSort::Title).await?;
        let author = self.sort_positions(BookSort::Author).await?;
        let ids: Vec<i64> = facts.books.keys().copied().collect();
        let summaries: HashMap<i64, (i64, i64)> = self
            .storage
            .book_summaries(&ids)
            .await?
            .into_iter()
            .map(|s| (s.book_id, (s.highlights, s.notes)))
            .collect();

        let mut counts = LibraryCounts::default();
        let mut total_pages = 0;
        let mut rows: Vec<LibraryRow> = facts
            .books
            .into_iter()
            .map(|(id, book)| {
                let latest = latest.remove(&id);
                counts.all += 1;
                match latest.as_ref().map(|r| &r.state) {
                    None => counts.no_reading += 1,
                    Some(ReadingState::Reading) => counts.reading += 1,
                    Some(ReadingState::Finished) => counts.read += 1,
                    Some(ReadingState::Abandoned) => counts.put_down += 1,
                    Some(ReadingState::Other(_)) => counts.other += 1,
                }
                total_pages += book.page_count.filter(|p| *p > 0).unwrap_or(0);
                let (passages, notes) = summaries.get(&id).copied().unwrap_or((0, 0));
                LibraryRow {
                    title_order: title.get(&id).copied().unwrap_or(i64::MAX),
                    author_order: author.get(&id).copied().unwrap_or(i64::MAX),
                    book,
                    latest,
                    passages,
                    notes,
                }
            })
            .collect();
        rows.sort_by_key(|r| (r.title_order, r.book.id));
        Ok(LibraryTable {
            rows,
            counts,
            total_pages,
        })
    }
}
