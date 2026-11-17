//! The Library table: every book in one sortable table.
//!
//! **The Library is plain data** — counts on the filters, every book in one
//! table, never-started books included. A frontend sorts and filters the whole
//! shelf in memory (hundreds of rows by design), so this answers the whole
//! shelf in one call, and what a frontend must not derive is on the row: the
//! latest read's state and its *Days*, the rating, the counts, and **the sort
//! positions** — see [`LibraryRow::title_order`] for why a position and not a
//! string.

use std::collections::HashMap;

use crate::Engine;
use crate::book::{Book, ReadingState};
use crate::error::Result;
use crate::history::{HistoryRead, summarise_reads};
use crate::storage::{BookSort, now_unix};

/// One row of the table.
#[derive(Debug, Clone)]
pub struct LibraryRow {
    pub book: Book,
    /// The book's 0-based position in the engine's **Title** order
    /// (`BookSort::Title`: `COALESCE(sort_title, title) COLLATE NOCASE`, the
    /// leading article dropped).
    ///
    /// **A position, not a key string.** The engine's filing key for authors is
    /// a packed tuple (`sort.rs`) that is kept off the wire on purpose, and a
    /// title key re-collated by JavaScript's `localeCompare` is a second
    /// collation that disagrees with SQLite's `NOCASE` on exactly the titles a
    /// reader notices. An integer sorts the same way in every language, so the
    /// frontend's in-memory sort *is* the engine's.
    pub title_order: i64,
    /// The same, for the engine's **Author** order (`BookSort::Author`: the
    /// first author's surname, then the whole name; authorless last).
    pub author_order: i64,
    /// The book's **current** read — the open one if any, else the most recent
    /// — chosen by the same rule `BOOK_FROM`'s join uses, so the row and every
    /// `BookDto` agree about which read is "the" read. `None` for a book with
    /// no reading at all, which the table shows like any other.
    pub latest: Option<HistoryRead>,
    /// Every passage of the book, every read.
    pub passages: i64,
    /// Every note on the book.
    pub notes: i64,
}

/// The filters' counts. They **partition** `all` (asserted): every book is in
/// exactly one of the five by its latest read's state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LibraryCounts {
    pub all: i64,
    pub reading: i64,
    pub read: i64,
    pub put_down: i64,
    /// A latest read whose status this build does not model (an importer's
    /// word). Shown verbatim on the row; a chip of its own only if non-zero.
    pub other: i64,
    pub no_reading: i64,
}

#[derive(Debug, Clone)]
pub struct LibraryTable {
    /// In Title order.
    pub rows: Vec<LibraryRow>,
    pub counts: LibraryCounts,
    /// Pages across every book with a page count — *18 books · 5,260 pages*.
    pub total_pages: i64,
}

/// The current read of each book: open first, then the latest start
/// (`COALESCE(started_at, created_at)`), then the higher id — `BOOK_FROM`'s
/// rule, restated over rows already in hand.
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
    /// The whole shelf as one table. See [`LibraryRow`].
    pub async fn library_table(&self) -> Result<LibraryTable> {
        let mut facts = self.history_facts(false).await?;
        // The table is the library: a book not in the library (no copy still
        // had, and not a touched book with any copy) is no row.
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
