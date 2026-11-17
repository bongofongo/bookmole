//! Ranked search over books: candidates from the trigram index (a prefix `LIKE` when no word has
//! three characters, the ISBN columns for an ISBN), narrowed by filter and scope in the same
//! statement before the cut, then ranked in Rust. The query is private: never logged above
//! `trace!`.

use std::collections::BTreeSet;

use sqlx::Row;

use super::books::{BOOK_COLUMNS, bind_all, book_current_join, row_to_book};
use super::query::{Bind, BookFilter, LibraryScope};
use super::{Storage, in_library_sql};
use crate::book::{Book, isbn10_to_13};
use crate::error::Result;
use crate::search::query::{KnowsNothing, ParsedQuery, parse};

const CANDIDATES_PER_RESULT: usize = 8;
const MIN_CANDIDATES: usize = 200;
const MAX_TRIGRAMS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BookMatch {
    pub book_id: i64,
    /// Comparable within one answer and with a provider result's score for the same query.
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldFacts {
    pub book_id: i64,
    /// In the library by the copies rule (`in_library_sql`).
    pub in_library: bool,
    pub has_copy: bool,
    pub passages: i64,
    pub notes: i64,
    pub lists: Vec<(i64, String)>,
}

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

#[derive(Debug, Clone, Default)]
pub(crate) struct LibraryIdentities {
    pub books: Vec<LibraryBook>,
    pub external: Vec<(String, String, i64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ask {
    Nothing,
    Isbn {
        thirteen: String,
        ten: Option<String>,
    },
    Prefix(String),
    Trigrams(String),
}

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

/// Distinct trigrams of words of three or more characters, sorted so one query is one statement.
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
        // Each trigram is a quoted phrase: `OR` and `NEAR` are words too.
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

/// Escapes `LIKE` wildcards, `\` first; the clause declares `\` as the escape.
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
    /// At most `limit` ids with scores, best first. `filter` and `scope` narrow candidates in SQL
    /// before the cut; an empty query issues no statement.
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

    /// One statement for all of `ids`, in the order asked; an id with no book is left out.
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
        assert_eq!(ask_of(&q("dune herbert")), ask_of(&q("herbert dune")));
    }
}

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

    /// fts5's integrity check (`rank = 1`): the only assertion that sees the index drifting from its
    /// table.
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

    /// Filter and scope narrow candidates before the cut: under bm25 250 short titles outrank the one
    /// long one.
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
        // A note is a touch, so the book stays in the library by the copies rule.
        assert!(gone_f.has_copy && gone_f.in_library);
        assert_eq!(gone_f.notes, 1);
        assert!(s.held_facts(&[]).await.unwrap().is_empty());

        let lib = s.library_identities().await.unwrap();
        let member: Vec<(i64, bool)> = lib.books.iter().map(|b| (b.id, b.in_library)).collect();
        assert_eq!(member, vec![(kept, true), (gone, true), (bare, false)]);
    }
}
