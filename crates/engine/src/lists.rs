//! Lists (migration `0028`): the public types.
//!
//! The rules live in [`crate::storage`]'s `lists.rs`; this file is the shapes a
//! frontend reads. A list's name, description and notes are the reader's own
//! writing and are never logged above `trace!`.

use crate::book::Book;

/// A list as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookList {
    pub id: i64,
    pub name: String,
    /// `None` is none; never `""`.
    pub description: Option<String>,
    /// `"user"`, or `"goodreads"` for a kept shelf.
    pub source: String,
    /// For a kept shelf, the tag it was made from. Provenance only.
    pub source_ref: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A list with the facts a home page draws.
#[derive(Debug, Clone)]
pub struct ListSummary {
    pub list: BookList,
    pub book_count: i64,
    /// Books on the list that are not in the library (see
    /// [`ListEntry::in_library`] for the rule).
    pub not_in_library: i64,
    /// Books on the list that are in the library, counted by the same rule in
    /// the same query, so `in_library + not_in_library == book_count`.
    pub in_library: i64,
    /// The first `preview` books by position.
    pub preview: Vec<Book>,
}

/// One book on one list.
#[derive(Debug, Clone)]
pub struct ListEntry {
    pub book: Book,
    /// 0-based within the list, whatever order the entry was read in.
    pub position: i64,
    pub note: Option<String>,
    pub added_at: i64,
    /// The year the list's Timeline would place it at (`None` = no year).
    pub year: Option<i64>,
    /// True when `year` is an edition's year: `first_published` is unknown and
    /// the year fell back to `publish_year`. False for a first-publication
    /// year, and when there is no year at all.
    pub year_is_edition: bool,
    /// In the library: a copy not ended, or any copy at all and the book
    /// touched. A book only on a list is not in the library, but still exists.
    pub in_library: bool,
}

/// Which lists a book is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListMembership {
    pub list_id: i64,
    pub name: String,
    /// 1-based ("No. 1 of 6").
    pub position: i64,
    pub list_size: i64,
    pub note: Option<String>,
}

/// A list's books laid along a line.
#[derive(Debug, Clone)]
pub struct ListTimeline {
    /// Books with a year, in the list's own order.
    pub dated: Vec<ListEntry>,
    /// Books without one, in the list's own order. They go last.
    pub undated: Vec<ListEntry>,
    /// At least [`TIMELINE_MIN_DATED`] books have a year.
    pub ready: bool,
}

/// How many dated books a Timeline needs to be worth drawing.
pub const TIMELINE_MIN_DATED: usize = 3;

/// A shelf from another system, offered as a list.
#[derive(Debug, Clone)]
pub struct ShelfSuggestion {
    /// `"goodreads"`.
    pub source: String,
    pub tag: String,
    /// The shelf as the reader wrote it (`raw`), else the tag.
    pub name: String,
    pub book_count: i64,
    pub preview: Vec<Book>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListSort {
    Name,
    #[default]
    RecentlyChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntryShow {
    #[default]
    All,
    InLibrary,
    NotInLibrary,
}

/// A view over a list's entries. **None of these rewrites `position`.**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntrySort {
    #[default]
    Yours,
    Published,
    Title,
}
