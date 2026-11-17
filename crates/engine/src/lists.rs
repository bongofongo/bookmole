//! Lists: the public types. Names, descriptions and notes are the reader's own writing,
//! never logged above `trace!`.

use crate::book::Book;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookList {
    pub id: i64,
    pub name: String,
    /// `None` is none; never `""`.
    pub description: Option<String>,
    /// `"user"`, or `"goodreads"` for a kept shelf.
    pub source: String,
    pub source_ref: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct ListSummary {
    pub list: BookList,
    pub book_count: i64,
    pub not_in_library: i64,
    pub in_library: i64,
    pub preview: Vec<Book>,
}

#[derive(Debug, Clone)]
pub struct ListEntry {
    pub book: Book,
    /// 0-based within the list, whatever order the entry was read in.
    pub position: i64,
    pub note: Option<String>,
    pub added_at: i64,
    pub year: Option<i64>,
    /// True when `year` is an edition's year: `first_published` is unknown and
    /// the year fell back to `publish_year`. False for a first-publication
    /// year, and when there is no year at all.
    pub year_is_edition: bool,
    /// In the library: a copy not ended, or any copy at all and the book
    /// touched. A book only on a list is not in the library, but still exists.
    pub in_library: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListMembership {
    pub list_id: i64,
    pub name: String,
    /// 1-based ("No. 1 of 6").
    pub position: i64,
    pub list_size: i64,
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ListTimeline {
    pub dated: Vec<ListEntry>,
    pub undated: Vec<ListEntry>,
    pub ready: bool,
}

pub const TIMELINE_MIN_DATED: usize = 3;

#[derive(Debug, Clone)]
pub struct ShelfSuggestion {
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
