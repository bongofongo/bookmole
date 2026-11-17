//! Your own books, ranked against a query. Nothing here touches the network:
//! the Library's search box and *Add a book*'s "Yours" half both answer from
//! this alone.
//!
//! `Storage::search_books` reads the candidates off the trigram index; this
//! decides which of them answer and in what order. **The order is
//! [`ranking::rank`]'s** — the function provider results are scored by, less
//! the provider-only signals (standing, agreement, position), which a book on
//! the shelf has none of — so the Library and *Add a book* cannot disagree
//! about what a good match is.
//!
//! **Membership is not a score threshold alone**, because the index is asked
//! with an OR of trigrams and answers broadly. A book answers when every
//! content word of three letters or more is *in* it, folded — infix, so
//! `possess` finds *The Dispossessed*, which is the one thing `BookFilter`'s
//! doc comment prized and a word-level score cannot see — **or** when it scores
//! at least [`TYPO_FLOOR`], which is how `midlemarch` still finds
//! *Middlemarch* though no word of it is.

use super::parsed::ResultNames;
use super::query::{KnowsNothing, ParsedQuery, fold, is_filler, parse};
use super::ranking::{Candidate, IDENTITY, rank};
use crate::book::Book;
use crate::providers::WorkFacts;

/// The score a book that does not hold every word of the query must reach to
/// answer it: a misspelling of its title, not a coincidence of letters.
/// `persuasionn` reaches *Persuasion* at about 66 and *Precaution* at about 36.
pub(crate) const TYPO_FLOOR: f64 = 50.0;

/// A shelf book's position, for ranking: none. `rank`'s position term is
/// `4 / (1 + position)`, so this is zero to every digit that is printed.
const NO_POSITION: usize = usize::MAX;

/// What ranking reads off a book on the shelf: its series and language. No
/// edition count and no other titles — those are a provider's facts.
pub(crate) fn facts_of(book: &Book) -> WorkFacts {
    WorkFacts {
        series: book.series.clone(),
        series_position: book.series_index,
        languages: book.language.iter().cloned().collect(),
        ..WorkFacts::default()
    }
}

/// One shelf book's score for `q`.
pub(crate) fn score_book(q: &ParsedQuery, book: &Book) -> f64 {
    let facts = facts_of(book);
    rank(
        q,
        &Candidate {
            book,
            facts: &facts,
            sources: &[],
            best_position: NO_POSITION,
            notability: None,
        },
    )
}

/// Whether `book`, scored `score`, answers `q` at all (the module doc).
pub(crate) fn answers(q: &ParsedQuery, book: &Book, score: f64) -> bool {
    if q.isbn.is_some() {
        return score >= IDENTITY;
    }
    let content: Vec<&String> = q
        .words
        .iter()
        .filter(|w| w.chars().count() >= 3 && !is_filler(w))
        .collect();
    if content.is_empty() {
        // Nothing long enough to look for: the index's prefix match decided.
        return true;
    }
    let mut hay = fold(book.title.as_deref().unwrap_or(""));
    for part in book
        .authors
        .iter()
        .chain(book.series.iter())
        .chain(book.isbn_13.iter())
        .chain(book.isbn_10.iter())
    {
        hay.push(' ');
        hay.push_str(&fold(part));
    }
    content.iter().all(|w| hay.contains(w.as_str())) || score >= TYPO_FLOOR
}

/// Rank `books` against `text`: those that answer, best first, at most
/// `limit`. Ties go to the lower id, so one library answers one way.
///
/// The text is read twice, as a search session reads it: once knowing nothing,
/// and once knowing the names these books carry, so `le guin dispossessed` is
/// believed as an author and a title once a Le Guin is among them. (A split of
/// the words into title and author is preferred when its author half is an
/// author the app already knows.)
pub(crate) fn rank_books(text: &str, books: Vec<Book>, limit: usize) -> Vec<(Book, f64)> {
    let first = parse(text, &KnowsNothing);
    let q = if first.isbn.is_some() {
        first
    } else {
        parse(text, &ResultNames::of(books.iter()))
    };
    let mut out: Vec<(Book, f64)> = books
        .into_iter()
        .filter_map(|b| {
            let s = score_book(&q, &b);
            answers(&q, &b, s).then_some((b, s))
        })
        .collect();
    out.sort_by(|(a, sa), (b, sb)| sb.total_cmp(sa).then(a.id.cmp(&b.id)));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(id: i64, title: &str, author: &str) -> Book {
        Book {
            id: Some(id),
            title: Some(title.into()),
            authors: vec![author.into()],
            ..Default::default()
        }
    }

    fn shelf() -> Vec<Book> {
        vec![
            book(1, "The Dispossessed", "Ursula K. Le Guin"),
            book(2, "Persuasion", "Jane Austen"),
            book(3, "Precaution", "James Fenimore Cooper"),
            book(4, "Middlemarch", "George Eliot"),
            book(5, "The Left Hand of Darkness", "Ursula K. Le Guin"),
            book(6, "Dune", "Frank Herbert"),
            book(7, "Dune Messiah", "Frank Herbert"),
        ]
    }

    fn ids(text: &str) -> Vec<i64> {
        rank_books(text, shelf(), 10)
            .into_iter()
            .filter_map(|(b, _)| b.id)
            .collect()
    }

    #[test]
    fn infix_and_typos_answer_and_coincidences_do_not() {
        assert_eq!(ids("possess"), vec![1], "infix");
        assert_eq!(ids("persuasionn"), vec![2], "a typo, and not Precaution");
        assert_eq!(ids("midlemarch"), vec![4]);
        assert_eq!(ids("dune"), vec![6, 7], "Dune above Dune Messiah");
        assert_eq!(ids("le guin")[..2], [1, 5], "an author's books");
        assert!(ids("zzyzx").is_empty());
    }

    #[test]
    fn an_isbn_answers_only_with_its_book() {
        let mut books = shelf();
        books[1].isbn_13 = Some("9781900000178".into());
        let got: Vec<i64> = rank_books("978-1-900000-17-8", books, 10)
            .into_iter()
            .filter_map(|(b, _)| b.id)
            .collect();
        assert_eq!(got, vec![2]);
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    fn arb_book() -> impl Strategy<Value = Book> {
        (
            1i64..1000,
            "[a-zA-Z ]{1,24}",
            proptest::collection::vec("[A-Z][a-z]{2,8} [A-Z][a-z]{2,8}", 0..2),
        )
            .prop_map(|(id, title, authors)| Book {
                id: Some(id),
                title: Some(title),
                authors,
                ..Default::default()
            })
    }

    proptest! {
        /// Ranking is invariant to the order the index returned the
        /// candidates in: one library answers one way.
        #[test]
        fn the_answer_does_not_depend_on_candidate_order(
            books in proptest::collection::vec(arb_book(), 0..12),
            text in "[a-z ]{0,16}",
            seed in any::<u64>(),
        ) {
            let mut shuffled = books.clone();
            let mut s = seed;
            for i in (1..shuffled.len()).rev() {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                shuffled.swap(i, (s >> 33) as usize % (i + 1));
            }
            let key = |v: Vec<(Book, f64)>| -> Vec<(Option<i64>, u64)> {
                v.into_iter().map(|(b, s)| (b.id, s.to_bits())).collect()
            };
            prop_assert_eq!(key(rank_books(&text, books, 50)), key(rank_books(&text, shuffled, 50)));
        }

        /// A book holding the whole query, folded, always answers it; and the
        /// answer is best first, finite, and never longer than asked.
        #[test]
        fn a_book_holding_the_query_answers_it(
            books in proptest::collection::vec(arb_book(), 1..12),
            pick in any::<proptest::sample::Index>(),
            limit in 1usize..20,
        ) {
            let target = pick.get(&books).clone();
            let text = target.title.clone().unwrap_or_default();
            let all = rank_books(&text, books.clone(), usize::MAX);
            prop_assert!(
                fold(&text).is_empty() || all.iter().any(|(b, _)| b.id == target.id),
                "{:?} did not answer its own title", target.title
            );
            let some = rank_books(&text, books, limit);
            prop_assert!(some.len() <= limit);
            prop_assert!(some.iter().all(|(_, s)| s.is_finite() && *s >= 0.0));
            prop_assert!(some.windows(2).all(|w| w[0].1 >= w[1].1));
        }
    }
}
