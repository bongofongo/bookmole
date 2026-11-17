//! A ranked search over the books (migration `0033`), and the reads *you have
//! this* stands on: the library read once per search into the form the
//! matcher compares against, never once per result row.
//!
//! **Candidates come from the index, the order from Rust.** The trigram index
//! answers *which books share the query's letters* — an OR of the query's
//! trigrams, so a typo still shares most of them; a prefix `LIKE` when no word
//! is three characters long, since a trigram index cannot see a shorter one;
//! the two ISBN columns exactly, for an ISBN. The caller's [`BookFilter`] and
//! [`LibraryScope`] narrow them **in the same statement, before the cut**: a
//! filter applied after `LIMIT` returns nothing whenever the best few hundred
//! hits are elsewhere (the same truncation `search_marks` guards against).
//! What is left is ranked by `search::local`, which is
//! `search::ranking::rank` — the scorer provider results get — so searching
//! your own library and searching to add a book cannot disagree about what a
//! good match is.
//!
//! The query is the reader's private interest: nothing here logs it, or
//! anything derived from it, above `trace!`.

use std::collections::BTreeSet;

use sqlx::Row;

use super::books::{BOOK_COLUMNS, bind_all, book_current_join, row_to_book};
use super::query::{Bind, BookFilter, LibraryScope};
use super::{Storage, in_library_sql};
use crate::book::{Book, isbn10_to_13};
use crate::error::Result;
use crate::search::query::{KnowsNothing, ParsedQuery, parse};

/// Candidates read per result asked for, before ranking. bm25 over trigrams is
/// a good first cut and a poor final order, so the cut is generous.
const CANDIDATES_PER_RESULT: usize = 8;
/// At least this many candidates, however few results are asked for: the
/// ranker, not bm25, decides which ones survive.
const MIN_CANDIDATES: usize = 200;
/// The most trigrams one `MATCH` names: a pasted paragraph is not a query, and
/// sixty-four trigrams of it find whatever it is about.
const MAX_TRIGRAMS: usize = 64;

/// One book a search found, and how well it answers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BookMatch {
    pub book_id: i64,
    /// [`crate::search::ranking::rank`]'s score. Comparable within one answer,
    /// and with a provider result's score for the same query.
    pub score: f64,
}

/// What the app holds of one book, for a search's *books you already have*
/// section (in the library or not, its copies, passages, notes and lists):
/// read for a whole answer in one statement ([`Storage::held_facts`]), never
/// per row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldFacts {
    pub book_id: i64,
    /// In the library by the copies rule (`in_library_sql`).
    pub in_library: bool,
    /// Any copy recorded at all, had now or not.
    pub has_copy: bool,
    /// Passages kept from it (highlights of every origin).
    pub passages: i64,
    /// Notes written about it, of every kind.
    pub notes: i64,
    /// The lists it is on, by name.
    pub lists: Vec<(i64, String)>,
}

/// One book as *you have this* reads it: its identities and its words.
#[derive(Debug, Clone)]
pub(crate) struct LibraryBook {
    pub id: i64,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub isbn_13: Option<String>,
    pub isbn_10: Option<String>,
    pub openlibrary_key: Option<String>,
    pub googlebooks_id: Option<String>,
    pub in_library: bool,
}

/// Every book the app holds, and every other system's id for one — what a
/// search prepares once (`search::held::Library`).
#[derive(Debug, Clone, Default)]
pub(crate) struct LibraryIdentities {
    pub books: Vec<LibraryBook>,
    /// `(source, external_id, book_id)` from `external_ids`.
    pub external: Vec<(String, String, i64)>,
}

/// How the index is asked, for one parsed query.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ask {
    /// Nothing to ask with: no statement is issued.
    Nothing,
    /// The two ISBN columns, exactly: the ISBN-13 and, where it has one, its
    /// ISBN-10.
    Isbn {
        thirteen: String,
        ten: Option<String>,
    },
    /// A prefix of the title, or of a word in it.
    Prefix(String),
    /// The trigram index, as an fts5 `MATCH` expression.
    Trigrams(String),
}

/// The ISBN-10 of a `978` ISBN-13, which is the only kind that has one.
fn isbn13_to_10(isbn13: &str) -> Option<String> {
    let body = isbn13.strip_prefix("978")?.get(..9)?;
    let mut sum = 0u32;
    for (i, c) in body.chars().enumerate() {
        sum += c.to_digit(10)? * (10 - i as u32);
    }
    let check = (11 - sum % 11) % 11;
    let check = if check == 10 {
        'X'
    } else {
        char::from_digit(check, 10)?
    };
    Some(format!("{body}{check}"))
}

/// The distinct trigrams of every word of three characters or more, sorted so
/// one query is one statement however its words were ordered.
fn trigrams(words: &[String]) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for w in words {
        let chars: Vec<char> = w.chars().collect();
        for t in chars.windows(3) {
            out.insert(t.iter().collect());
        }
    }
    out.into_iter().take(MAX_TRIGRAMS).collect()
}

fn ask_of(q: &ParsedQuery) -> Ask {
    if let Some(isbn) = &q.isbn {
        let thirteen = if isbn.len() == 13 {
            Some(isbn.clone())
        } else {
            isbn10_to_13(isbn)
        };
        return match thirteen {
            Some(thirteen) => Ask::Isbn {
                ten: isbn13_to_10(&thirteen),
                thirteen,
            },
            None => Ask::Nothing,
        };
    }
    let grams = trigrams(&q.words);
    if !grams.is_empty() {
        // Every trigram a quoted phrase (fts.rs' rule): a folded word holds
        // only letters and digits, but `OR` and `NEAR` are words too.
        let expr = grams
            .iter()
            .map(|g| format!("\"{}\"", g.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        return Ask::Trigrams(expr);
    }
    let short = q.words.join(" ");
    if short.is_empty() {
        Ask::Nothing
    } else {
        Ask::Prefix(short)
    }
}

/// `LIKE`'s wildcards escaped, `\` first (the clause declares it the escape).
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

impl Storage {
    /// Search the books for `query`, best match first: ids and scores, at most
    /// `limit` of them.
    ///
    /// `filter` is the caller's [`BookFilter`] and `scope` which books count —
    /// a search of your library asks with [`LibraryScope::InLibrary`], a
    /// search to add a book with [`LibraryScope::All`]. Both narrow the candidates in SQL, before the
    /// cut. An empty query is no hits and no statement.
    pub async fn search_books(
        &self,
        query: &str,
        filter: &BookFilter,
        scope: LibraryScope,
        limit: usize,
    ) -> Result<Vec<BookMatch>> {
        Ok(self
            .search_books_ranked(query, filter, scope, limit)
            .await?
            .into_iter()
            .filter_map(|(book, score)| book.id.map(|book_id| BookMatch { book_id, score }))
            .collect())
    }

    /// [`Storage::search_books`], with the books themselves — what a row for a
    /// book you already have shows, read in the same statement that found it.
    pub(crate) async fn search_books_ranked(
        &self,
        query: &str,
        filter: &BookFilter,
        scope: LibraryScope,
        limit: usize,
    ) -> Result<Vec<(Book, f64)>> {
        let parsed = parse(query, &KnowsNothing);
        let candidates = self
            .book_candidates(&parsed, filter, scope, limit.max(1))
            .await?;
        tracing::debug!(candidates = candidates.len(), "books searched");
        Ok(crate::search::local::rank_books(query, candidates, limit))
    }

    async fn book_candidates(
        &self,
        q: &ParsedQuery,
        filter: &BookFilter,
        scope: LibraryScope,
        limit: usize,
    ) -> Result<Vec<Book>> {
        let ask = ask_of(q);
        let (mut clauses, mut binds, from, order): (Vec<String>, Vec<Bind>, String, &str) =
            match &ask {
                Ask::Nothing => return Ok(Vec::new()),
                Ask::Isbn { thirteen, ten } => {
                    let mut c = "books.isbn_13 = ?".to_string();
                    let mut b = vec![Bind::Text(thirteen.clone())];
                    if let Some(ten) = ten {
                        c = format!("({c} OR books.isbn_10 = ?)");
                        b.push(Bind::Text(ten.clone()));
                    }
                    (
                        vec![c],
                        b,
                        concat!("FROM books ", book_current_join!()).to_string(),
                        "books.id",
                    )
                }
                Ask::Prefix(p) => {
                    let p = like_escape(p);
                    (
                        vec![
                            r"(books.title LIKE ? ESCAPE '\' OR books.title LIKE ? ESCAPE '\')"
                                .to_string(),
                        ],
                        vec![Bind::Text(format!("{p}%")), Bind::Text(format!("% {p}%"))],
                        concat!("FROM books ", book_current_join!()).to_string(),
                        "books.id",
                    )
                }
                Ask::Trigrams(expr) => (
                    vec!["books_search MATCH ?".to_string()],
                    vec![Bind::Text(expr.clone())],
                    concat!(
                        "FROM books_search JOIN books ON books.id = books_search.rowid ",
                        book_current_join!()
                    )
                    .to_string(),
                    "books_search.rank, books.id",
                ),
            };
        let predicate = filter.predicate();
        if let Some(rest) = predicate.sql.strip_prefix("WHERE ") {
            clauses.push(format!("({rest})"));
            binds.extend(predicate.binds);
        }
        if let Some(c) = scope.clause("books.id") {
            clauses.push(c);
        }
        let cut = (limit.saturating_mul(CANDIDATES_PER_RESULT)).max(MIN_CANDIDATES);
        let sql = format!(
            "SELECT {BOOK_COLUMNS} {from} WHERE {} ORDER BY {order} LIMIT ?",
            clauses.join(" AND ")
        );
        let rows = bind_all(sqlx::query(&sql), &binds)
            .bind(i64::try_from(cut).unwrap_or(i64::MAX))
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_book).collect()
    }

    /// What the app holds of each of `ids`, in the order asked, in **one**
    /// statement: in the library or not, any copy, how many passages and
    /// notes, and the lists the book is on. An id with no book is left out.
    pub(crate) async fn held_facts(&self, ids: &[i64]) -> Result<Vec<HeldFacts>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT j.value AS id,
                    {} AS in_library,
                    EXISTS (SELECT 1 FROM copies hc WHERE hc.book_id = j.value) AS has_copy,
                    (SELECT count(*) FROM highlights hh WHERE hh.book_id = j.value) AS passages,
                    (SELECT count(*) FROM notes hn WHERE hn.book_id = j.value) AS notes,
                    (SELECT json_group_array(json_array(l.id, l.name)) FROM
                        (SELECT bl.id, bl.name FROM book_list_entries e
                           JOIN book_lists bl ON bl.id = e.list_id
                          WHERE e.book_id = j.value
                          ORDER BY bl.name COLLATE NOCASE, bl.id) l) AS lists
               FROM json_each(?) j
              WHERE EXISTS (SELECT 1 FROM books b WHERE b.id = j.value)
              ORDER BY j.key",
            in_library_sql("j.value")
        );
        let rows = sqlx::query(&sql)
            .bind(serde_json::to_string(ids).unwrap_or_else(|_| "[]".into()))
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| {
                let lists: String = r.try_get("lists")?;
                let lists: Vec<(i64, String)> = serde_json::from_str(&lists).unwrap_or_default();
                Ok(HeldFacts {
                    book_id: r.try_get("id")?,
                    in_library: r.try_get("in_library")?,
                    has_copy: r.try_get("has_copy")?,
                    passages: r.try_get("passages")?,
                    notes: r.try_get("notes")?,
                    lists,
                })
            })
            .collect()
    }

    /// Every book's identities and words, and every external id: the library
    /// as a search prepares it, once per search and never once per row. Two
    /// statements, whatever the size of the library.
    pub(crate) async fn library_identities(&self) -> Result<LibraryIdentities> {
        let sql = format!(
            "SELECT books.id, books.title, books.authors, books.isbn_13, books.isbn_10,
                    books.openlibrary_key, books.googlebooks_id, {} AS in_library
               FROM books ORDER BY books.id",
            in_library_sql("books.id")
        );
        let rows = sqlx::query(&sql).fetch_all(self.pool()).await?;
        let books = rows
            .iter()
            .map(|r| {
                let authors: String = r.try_get("authors")?;
                let title: String = r.try_get("title")?;
                Ok(LibraryBook {
                    id: r.try_get("id")?,
                    title: Some(title).filter(|t| !t.is_empty()),
                    authors: serde_json::from_str(&authors).unwrap_or_default(),
                    isbn_13: r.try_get("isbn_13")?,
                    isbn_10: r.try_get("isbn_10")?,
                    openlibrary_key: r.try_get("openlibrary_key")?,
                    googlebooks_id: r.try_get("googlebooks_id")?,
                    in_library: r.try_get("in_library")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let external = sqlx::query("SELECT source, external_id, book_id FROM external_ids")
            .fetch_all(self.pool())
            .await?
            .iter()
            .map(|r| {
                Ok((
                    r.try_get("source")?,
                    r.try_get("external_id")?,
                    r.try_get("book_id")?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(LibraryIdentities { books, external })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(text: &str) -> ParsedQuery {
        parse(text, &KnowsNothing)
    }

    #[test]
    fn an_isbn_ten_is_derived_only_from_978() {
        assert_eq!(isbn13_to_10("9780441172719").as_deref(), Some("0441172717"));
        assert_eq!(isbn13_to_10("9780316769488").as_deref(), Some("0316769487"));
        assert_eq!(isbn13_to_10("9791032305690"), None);
    }

    #[test]
    fn the_index_is_asked_the_way_the_query_reads() {
        assert_eq!(ask_of(&q("")), Ask::Nothing);
        assert_eq!(ask_of(&q("It")), Ask::Prefix("it".into()));
        assert_eq!(
            ask_of(&q("978-0-441-17271-9")),
            Ask::Isbn {
                thirteen: "9780441172719".into(),
                ten: Some("0441172717".into())
            }
        );
        let Ask::Trigrams(expr) = ask_of(&q("Dune")) else {
            panic!("a word of four letters is two trigrams")
        };
        assert_eq!(expr, "\"dun\" OR \"une\"");
        // The order the words were typed in does not change the statement.
        assert_eq!(ask_of(&q("dune herbert")), ask_of(&q("herbert dune")));
    }
}

/// The index against the real writers, in a real (in-memory) database.
#[cfg(test)]
mod storage_tests {
    use super::*;
    use crate::storage::Source;

    async fn storage() -> Storage {
        Storage::connect("sqlite::memory:").await.unwrap()
    }

    fn book(title: &str, author: &str) -> Book {
        Book {
            title: Some(title.into()),
            authors: vec![author.into()],
            ..Default::default()
        }
    }

    async fn found(s: &Storage, q: &str) -> Vec<i64> {
        s.search_books(q, &BookFilter::default(), LibraryScope::All, 50)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.book_id)
            .collect()
    }

    /// fts5's own check that the index holds exactly what the content table
    /// says — `rank = 1` makes it read `books` back and compare. The only
    /// assertion that can see an index drifting from its table.
    async fn index_is_true(s: &Storage) {
        sqlx::query("INSERT INTO books_search (books_search, rank) VALUES ('integrity-check', 1)")
            .execute(s.pool())
            .await
            .expect("books_search agrees with books");
    }

    async fn give_copy(s: &Storage, book: i64, have_now: Option<bool>) {
        sqlx::query(
            "INSERT INTO copies (book_id, form, tenure, have_now, learned_from, created_at, updated_at)
             VALUES (?, 'print', 'own', ?, 'user', 0, 0)",
        )
        .bind(book)
        .bind(have_now)
        .execute(s.pool())
        .await
        .unwrap();
    }

    /// Every writer of `books` keeps the index true, `merge_books` above all:
    /// a merge fills `dst` and deletes `src` in one transaction, and both
    /// halves have to reach the index without naming it.
    #[tokio::test]
    async fn every_books_writer_keeps_the_index_true() {
        let s = storage().await;
        let a = s
            .upsert_book(&book("The Dispossessed", "Ursula K. Le Guin"), None)
            .await
            .unwrap();
        index_is_true(&s).await;
        assert_eq!(found(&s, "possess").await, vec![a], "infix");
        assert_eq!(found(&s, "le guin").await, vec![a], "authors");

        // enrich (incoming wins), fill (stored wins), a user's edit.
        s.enrich_book(
            a,
            &Book {
                series: Some("Hainish Cycle".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        index_is_true(&s).await;
        assert_eq!(found(&s, "hainish").await, vec![a], "series");
        s.fill_book(
            a,
            &Book {
                isbn_13: Some("9780060512750".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        index_is_true(&s).await;
        assert_eq!(found(&s, "9780060512750").await, vec![a], "an ISBN");
        assert_eq!(found(&s, "0060512750").await, vec![a], "its ISBN-10");
        s.enrich_book(a, &book("Anarres", "Ursula K. Le Guin"), Some(Source::User))
            .await
            .unwrap();
        index_is_true(&s).await;
        assert!(found(&s, "possess").await.is_empty(), "the old title left");
        assert_eq!(found(&s, "anarres").await, vec![a]);

        // A merge: `src` goes, `dst` takes its ISBN.
        let src = s
            .upsert_book(
                &Book {
                    isbn_13: Some("9781900000178".into()),
                    ..book("Middlemarch", "George Eliot")
                },
                None,
            )
            .await
            .unwrap();
        let dst = s
            .upsert_book(&book("Middlemarch (novel)", "George Eliot"), None)
            .await
            .unwrap();
        assert_eq!(found(&s, "middlemarch").await.len(), 2);
        s.merge_books(src, dst).await.unwrap();
        index_is_true(&s).await;
        assert_eq!(found(&s, "middlemarch").await, vec![dst]);
        assert_eq!(found(&s, "978-1-900000-17-8").await, vec![dst]);

        s.delete_book(dst).await.unwrap();
        index_is_true(&s).await;
        assert!(found(&s, "middlemarch").await.is_empty());
        assert!(found(&s, "9781900000178").await.is_empty());
    }

    /// The caller's filter and scope narrow the candidates **before** the cut.
    /// Two hundred and fifty short titles outrank the one long one under bm25,
    /// so a scope applied after `LIMIT` would find nothing.
    #[tokio::test]
    async fn scope_and_filter_narrow_before_the_cut() {
        let s = storage().await;
        for i in 0..(MIN_CANDIDATES + 50) {
            s.upsert_book(&book(&format!("Dune {i}"), "Somebody"), None)
                .await
                .unwrap();
        }
        let kept = s
            .upsert_book(
                &book(
                    "A long and winding account of dune ecology and many other things",
                    "Liet Kynes",
                ),
                None,
            )
            .await
            .unwrap();
        give_copy(&s, kept, None).await;
        let got = s
            .search_books("dune", &BookFilter::default(), LibraryScope::InLibrary, 5)
            .await
            .unwrap();
        assert_eq!(
            got.iter().map(|m| m.book_id).collect::<Vec<_>>(),
            vec![kept]
        );
        let by_author = BookFilter {
            author: Some("Kynes".into()),
            ..Default::default()
        };
        let got = s
            .search_books("dune", &by_author, LibraryScope::All, 5)
            .await
            .unwrap();
        assert_eq!(
            got.iter().map(|m| m.book_id).collect::<Vec<_>>(),
            vec![kept]
        );
        // And with nothing narrowing, the count asked for is the count given.
        let all = s
            .search_books("dune", &BookFilter::default(), LibraryScope::All, 5)
            .await
            .unwrap();
        assert_eq!(all.len(), 5);
    }

    #[tokio::test]
    async fn a_two_letter_title_is_found_by_its_prefix() {
        let s = storage().await;
        let it = s
            .upsert_book(&book("It", "Stephen King"), None)
            .await
            .unwrap();
        s.upsert_book(&book("Emma", "Jane Austen"), None)
            .await
            .unwrap();
        assert_eq!(found(&s, "it").await, vec![it]);
        assert!(
            found(&s, "   ").await.is_empty(),
            "nothing asked, nothing found"
        );
    }

    /// One statement for a whole section: membership, copies, passages,
    /// notes and lists, in the order asked, a missing id left out.
    #[tokio::test]
    async fn held_facts_come_back_for_a_whole_section() {
        let s = storage().await;
        let kept = s
            .upsert_book(&book("Circe", "Madeline Miller"), None)
            .await
            .unwrap();
        let gone = s
            .upsert_book(&book("Emma", "Jane Austen"), None)
            .await
            .unwrap();
        let bare = s
            .upsert_book(&book("Beloved", "Toni Morrison"), None)
            .await
            .unwrap();
        give_copy(&s, kept, None).await;
        give_copy(&s, gone, Some(false)).await;
        for (b, t) in [(kept, "one"), (kept, "two")] {
            sqlx::query(
                "INSERT INTO highlights (book_id, text, identity_hash, created_at) VALUES (?, ?, ?, 0)",
            )
            .bind(b)
            .bind(t)
            .bind(t)
            .execute(s.pool())
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO notes (book_id, file_path, title, created_at, last_modified)
             VALUES (?, 'n.md', 'n', 0, 0)",
        )
        .bind(gone)
        .execute(s.pool())
        .await
        .unwrap();
        let later = s.create_list("Later", None).await.unwrap();
        let again = s.create_list("Again", None).await.unwrap();
        s.add_to_list(later.id, kept, None).await.unwrap();
        s.add_to_list(again.id, kept, None).await.unwrap();

        let facts = s.held_facts(&[bare, kept, 999, gone]).await.unwrap();
        let ids: Vec<i64> = facts.iter().map(|f| f.book_id).collect();
        assert_eq!(ids, vec![bare, kept, gone]);
        let [bare_f, kept_f, gone_f] = &facts[..] else {
            unreachable!()
        };
        assert!(!bare_f.in_library && !bare_f.has_copy);
        assert!(kept_f.in_library && kept_f.has_copy);
        assert_eq!((kept_f.passages, kept_f.notes), (2, 0));
        assert_eq!(
            kept_f.lists,
            vec![
                (again.id, "Again".to_string()),
                (later.id, "Later".to_string())
            ]
        );
        // Not had any more, and untouched until the note: a note is a touch,
        // so the book stays (the copies rule).
        assert!(gone_f.has_copy && gone_f.in_library);
        assert_eq!(gone_f.notes, 1);
        assert!(s.held_facts(&[]).await.unwrap().is_empty());

        let lib = s.library_identities().await.unwrap();
        let member: Vec<(i64, bool)> = lib.books.iter().map(|b| (b.id, b.in_library)).collect();
        assert_eq!(member, vec![(kept, true), (gone, true), (bare, false)]);
    }
}
