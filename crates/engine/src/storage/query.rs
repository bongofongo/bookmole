//! `BookQuery` and `BookFilter`: one `WHERE` shared by `list_books` and `count_books` so a count
//! and a page agree. Paging is by offset, and every `ORDER BY` ends in a row id so pages form a
//! total order.

use crate::book::ReadingState;

use super::{BookSort, DayRange, ReadingSort};

/// Values travel beside the clause as binds, never interpolated.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Bind {
    Text(String),
    Int(i64),
}

/// `sql` is empty or starts with `WHERE `, so callers interpolate it without checking.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Predicate {
    pub(super) sql: String,
    pub(super) binds: Vec<Bind>,
}

/// Absence of a reading is a case here, deliberately not a `ReadingState` variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusFilter {
    /// `ReadingState::Other(raw)` passes through, so unmodelled device vocabulary is still
    /// filterable.
    Is(ReadingState),
    NoReading,
}

/// `Default` is every book, so filtered and unfiltered calls share one path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BookFilter {
    pub status: Option<StatusFilter>,
    /// Case-insensitive substring (`LIKE`), not FTS: it must compose with the count, every sort and
    /// offsets, and match infix.
    pub title: Option<String>,
    /// Case-insensitive substring of the stored JSON author list, not a join.
    pub author: Option<String>,
    pub year: Option<i64>,
    /// Matched whole, case-insensitively; `en-GB` is not `en`.
    pub language: Option<String>,
    /// A minted shelf in `book_tags`, matched whole and case-insensitively; not `books.subjects`.
    pub tag: Option<String>,
    /// `None` means no constraint, not a third answer.
    pub has_cover: Option<bool>,
    pub library: LibraryScope,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LibraryScope {
    #[default]
    All,
    InLibrary,
    NotInLibrary,
}

impl LibraryScope {
    pub(crate) fn clause(self, book_id: &str) -> Option<String> {
        match self {
            LibraryScope::All => None,
            LibraryScope::InLibrary => Some(super::in_library_sql(book_id)),
            LibraryScope::NotInLibrary => Some(format!("NOT {}", super::in_library_sql(book_id))),
        }
    }
}

/// Escapes LIKE wildcards; `\` is escaped first so later escapes are not themselves re-escaped.
fn like_escape(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len());
    for c in needle.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl BookFilter {
    pub fn is_empty(&self) -> bool {
        *self == BookFilter::default()
    }

    pub(super) fn predicate(&self) -> Predicate {
        let mut parts: Vec<String> = Vec::new();
        let mut binds: Vec<Bind> = Vec::new();

        match &self.status {
            Some(StatusFilter::Is(state)) => {
                // `cur.status` holds the stored vocabulary, so `Other(raw)` filters on the raw word.
                parts.push("cur.status = ?".into());
                binds.push(Bind::Text(state.to_string()));
            }
            // Not `status IS NULL`: a reading with a NULL status still counts as opened.
            Some(StatusFilter::NoReading) => parts.push("cur.id IS NULL".into()),
            None => {}
        }
        if let Some(title) = &self.title {
            parts.push(r"books.title LIKE ? ESCAPE '\'".into());
            binds.push(Bind::Text(format!("%{}%", like_escape(title))));
        }
        if let Some(author) = &self.author {
            parts.push(r"books.authors LIKE ? ESCAPE '\'".into());
            binds.push(Bind::Text(format!("%{}%", like_escape(author))));
        }
        if let Some(year) = self.year {
            parts.push("books.publish_year = ?".into());
            binds.push(Bind::Int(year));
        }
        if let Some(language) = &self.language {
            parts.push("books.language = ? COLLATE NOCASE".into());
            binds.push(Bind::Text(language.clone()));
        }
        if let Some(tag) = &self.tag {
            // EXISTS, not a join: a join would list a book twice for two sources and skew `count_books`.
            parts.push(
                "EXISTS (SELECT 1 FROM book_tags bt \
                 WHERE bt.book_id = books.id AND bt.tag = ? COLLATE NOCASE)"
                    .into(),
            );
            binds.push(Bind::Text(tag.clone()));
        }
        match self.has_cover {
            Some(true) => parts.push("books.cover_path IS NOT NULL".into()),
            Some(false) => parts.push("books.cover_path IS NULL".into()),
            None => {}
        }
        if let Some(clause) = self.library.clause("books.id") {
            parts.push(clause);
        }

        let sql = if parts.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", parts.join(" AND "))
        };
        Predicate { sql, binds }
    }
}

/// One struct because adjacent `limit` and `offset` `i64`s are a swap no type checker catches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookQuery {
    pub sort: BookSort,
    pub filter: BookFilter,
    /// Negative means no limit, as SQLite reads `LIMIT -1`.
    pub limit: i64,
    /// Negative is clamped to zero.
    pub offset: i64,
}

impl Default for BookQuery {
    fn default() -> Self {
        BookQuery {
            sort: BookSort::LastModified,
            filter: BookFilter::default(),
            limit: -1,
            offset: 0,
        }
    }
}

impl BookQuery {
    pub fn new(limit: i64, sort: BookSort) -> BookQuery {
        BookQuery {
            sort,
            limit,
            ..BookQuery::default()
        }
    }

    pub fn with_filter(mut self, filter: BookFilter) -> BookQuery {
        self.filter = filter;
        self
    }

    pub fn at_offset(mut self, offset: i64) -> BookQuery {
        self.offset = offset;
        self
    }
}

/// `Default` is every reading. Deliberately no `source` or `has_passage` filter: nothing asks for
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadingFilter {
    pub book_id: Option<i64>,
    /// `ReadingState::Other(raw)` passes through; there is no `NoReading` case since every row is a
    /// reading.
    pub status: Option<ReadingState>,
    /// Open means `finished_at IS NULL`, not a status: `abandon_reading` leaves a reading open.
    pub open: Option<bool>,
    /// Finished within this UTC day span; binds `unix_bounds` against the bare column.
    pub finished_in: Option<DayRange>,
}

impl ReadingFilter {
    pub fn is_empty(&self) -> bool {
        *self == ReadingFilter::default()
    }

    /// Nothing here names `books` or `cur`, so the count can drop the current-reading join.
    pub(super) fn predicate(&self) -> Predicate {
        let mut parts: Vec<String> = Vec::new();
        let mut binds: Vec<Bind> = Vec::new();

        if let Some(book_id) = self.book_id {
            parts.push("readings.book_id = ?".into());
            binds.push(Bind::Int(book_id));
        }
        if let Some(state) = &self.status {
            parts.push("readings.status = ?".into());
            binds.push(Bind::Text(state.to_string()));
        }
        match self.open {
            Some(true) => parts.push("readings.finished_at IS NULL".into()),
            Some(false) => parts.push("readings.finished_at IS NOT NULL".into()),
            None => {}
        }
        if let Some(range) = &self.finished_in {
            // Bare column, half-open: `date(finished_at, 'unixepoch')` is an expression
            // `idx_readings_finished_at` cannot serve.
            let (start, end) = range.unix_bounds();
            parts.push("readings.finished_at >= ? AND readings.finished_at < ?".into());
            binds.push(Bind::Int(start));
            binds.push(Bind::Int(end));
        }

        let sql = if parts.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", parts.join(" AND "))
        };
        Predicate { sql, binds }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingQuery {
    pub sort: ReadingSort,
    pub filter: ReadingFilter,
    /// Negative means no limit (`LIMIT -1`).
    pub limit: i64,
    /// Negative is clamped to zero.
    pub offset: i64,
}

impl Default for ReadingQuery {
    /// Most recently finished first, not by `last_modified`, which moves on device sync; open
    /// readings sort last.
    fn default() -> Self {
        ReadingQuery {
            sort: ReadingSort::Finished,
            filter: ReadingFilter::default(),
            limit: -1,
            offset: 0,
        }
    }
}

impl ReadingQuery {
    pub fn new(limit: i64, sort: ReadingSort) -> ReadingQuery {
        ReadingQuery {
            sort,
            limit,
            ..ReadingQuery::default()
        }
    }

    pub fn with_filter(mut self, filter: ReadingFilter) -> ReadingQuery {
        self.filter = filter;
        self
    }

    pub fn at_offset(mut self, offset: i64) -> ReadingQuery {
        self.offset = offset;
        self
    }
}

/// Counts rather than booleans: more information, and `has` is the one comparison.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BookSummary {
    pub book_id: i64,
    pub highlights: i64,
    pub notes: i64,
    pub files: i64,
}

impl BookSummary {
    pub fn has(&self) -> bool {
        self.highlights > 0 || self.notes > 0 || self.files > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_filter_writes_no_clause() {
        let p = BookFilter::default().predicate();
        assert!(p.sql.is_empty());
        assert!(p.binds.is_empty());
        assert!(BookFilter::default().is_empty());
    }

    #[test]
    fn no_reading_is_the_absent_join_and_not_a_null_status() {
        let p = BookFilter {
            status: Some(StatusFilter::NoReading),
            ..Default::default()
        }
        .predicate();
        assert_eq!(p.sql, "WHERE cur.id IS NULL");
        assert!(
            !p.sql.contains("status"),
            "no reading must not be spelled as a status value: {}",
            p.sql
        );
    }

    #[test]
    fn an_unknown_status_is_filterable_by_its_own_word() {
        let p = BookFilter {
            status: Some(StatusFilter::Is(ReadingState::Other("paused".into()))),
            ..Default::default()
        }
        .predicate();
        assert_eq!(p.binds, vec![Bind::Text("paused".into())]);
    }

    #[test]
    fn every_predicate_binds_in_the_order_it_names() {
        let p = BookFilter {
            status: Some(StatusFilter::Is(ReadingState::Reading)),
            title: Some("Dispossessed".into()),
            author: Some("Le Guin".into()),
            year: Some(1974),
            language: Some("en".into()),
            tag: Some("sf".into()),
            has_cover: Some(true),
            library: LibraryScope::All,
        }
        .predicate();
        assert_eq!(
            p.binds,
            vec![
                Bind::Text("reading".into()),
                Bind::Text("%Dispossessed%".into()),
                Bind::Text("%Le Guin%".into()),
                Bind::Int(1974),
                Bind::Text("en".into()),
                Bind::Text("sf".into()),
            ],
            "the clause names six values and `has_cover` names none"
        );
        // Not counted by splitting on " AND ": the tag clause contains one.
        for named in [
            "cur.status",
            "books.title LIKE",
            "books.authors LIKE",
            "books.publish_year",
            "books.language",
            "book_tags",
            "books.cover_path IS NOT NULL",
        ] {
            assert!(p.sql.contains(named), "{named} missing from {}", p.sql);
        }
        assert!(p.sql.starts_with("WHERE "));
    }

    #[test]
    fn a_wildcard_in_the_needle_is_a_literal() {
        assert_eq!(like_escape("100%"), r"100\%");
        assert_eq!(like_escape("a_b"), r"a\_b");
        assert_eq!(like_escape(r"a\b"), r"a\\b");
    }

    #[test]
    fn the_title_filter_matches_the_way_its_neighbours_do() {
        let title = BookFilter {
            title: Some("Dispossessed".into()),
            ..Default::default()
        }
        .predicate();
        let author = BookFilter {
            author: Some("Dispossessed".into()),
            ..Default::default()
        }
        .predicate();
        assert_eq!(
            title.sql.replace("books.title", "X"),
            author.sql.replace("books.authors", "X"),
            "one clause shape, two columns"
        );
        assert_eq!(title.binds, author.binds, "and one binding shape");
        assert!(
            !title.sql.contains("MATCH") && !title.sql.contains("fts"),
            "a filter is not a search: {}",
            title.sql
        );
        assert_eq!(
            BookFilter {
                title: Some("possess".into()),
                ..Default::default()
            }
            .predicate()
            .binds,
            vec![Bind::Text("%possess%".into())]
        );
    }

    #[test]
    fn the_default_query_is_the_whole_library_by_recency() {
        let q = BookQuery::default();
        assert!(q.limit < 0, "a negative limit is SQLite's own `no limit`");
        assert_eq!(q.offset, 0);
        assert_eq!(q.sort, BookSort::LastModified);
        assert!(q.filter.is_empty());
        assert_eq!(BookQuery::new(20, BookSort::Title).limit, 20);
    }
}
