//! "You have this": one question, one matcher, two strengths.
//!
//! - **[`Held::Certain`]**: one of the work's editions has an ISBN a book the
//!   app holds has, or the work carries a provider id a held book carries
//!   (`openlibrary_key`, `googlebooks_id`, or a stored external id under that
//!   provider's name). The row opens that book instead of adding one.
//! - **[`Held::Likely`]**: no identifier matches, and `matching.rs` — the one
//!   matcher every import path uses — links the work's title and authors to a
//!   held book with no question asked (`AUTO_MATCH`, the author veto and all).
//!   The row says *you may have this* and still offers to add; a false certain
//!   answer would open the wrong book.
//! - The candidate band below `AUTO_MATCH` is **not shown**: it exists to stop
//!   a silent duplicate on import, and on a search row the reader is already
//!   looking at both titles.
//!
//! "Holds" is **any book the app has a row for**, in the library or not (a
//! book only on a list is still yours to open, and adding it again would
//! make a duplicate). Library membership by the copies rule matters to one
//! thing here, the default edition: an edition *in the library* is the
//! one a work defaults to, and [`Library::in_library_isbns`] is that set.
//!
//! **The library is read and prepared once per search** ([`Library::prepare`]),
//! never once per row: `calibre.rs` records the cost of the other way.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::works::WorkResult;
use crate::matching::{Prepared, Query};
use crate::providers::ProviderId;
use crate::storage::LibraryIdentities;

/// What the app holds of a work.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Held {
    /// Nothing the app holds is this work, as far as anything can tell.
    #[default]
    None,
    /// An identifier says so: this book is that one.
    Certain(i64),
    /// The shared matcher says so with no question asked; no identifier does.
    /// `title` is the held book's, for *You may have this: <title>*.
    Likely { book_id: i64, title: Option<String> },
}

impl Held {
    /// The book either strength names.
    pub fn book_id(&self) -> Option<i64> {
        match self {
            Held::None => None,
            Held::Certain(id) | Held::Likely { book_id: id, .. } => Some(*id),
        }
    }

    /// The book, when an identifier says so — what may open it on Enter.
    pub fn certain(&self) -> Option<i64> {
        match self {
            Held::Certain(id) => Some(*id),
            _ => None,
        }
    }
}

/// The ISBN-13 of an ISBN as stored (either column).
fn thirteen(isbn: &str) -> Option<String> {
    if isbn.len() == 13 {
        Some(isbn.to_string())
    } else {
        crate::book::isbn10_to_13(isbn)
    }
}

/// The library, read once and prepared for every work of one search.
pub(crate) struct Library {
    by_isbn: HashMap<String, i64>,
    /// `(provider word, its id)` → book: the provider id columns and the
    /// external ids stored under a provider's name.
    by_provider_id: HashMap<(String, String), i64>,
    in_library_isbns: HashSet<String>,
    /// Each titled book, prepared for the matcher, with its title.
    titled: Vec<(i64, Option<String>, Prepared)>,
    /// Content word → the `titled` entries whose title has it. An auto match
    /// shares a word (`Prepared::words`), so only these are compared.
    by_word: HashMap<String, Vec<usize>>,
}

impl Library {
    /// Prepare what [`crate::Storage`]'s `library_identities` read. When two
    /// books claim one identifier (two rows of one ISBN cannot exist, but a
    /// work key can be on two), the lower id answers, so one library answers
    /// one way.
    pub(crate) fn prepare(ids: LibraryIdentities) -> Library {
        let mut by_isbn: BTreeMap<String, i64> = BTreeMap::new();
        let mut by_provider_id: BTreeMap<(String, String), i64> = BTreeMap::new();
        let mut in_library_isbns = HashSet::new();
        let mut titled = Vec::new();
        fn first<K: Ord>(map: &mut BTreeMap<K, i64>, key: K, id: i64) {
            let slot = map.entry(key).or_insert(id);
            *slot = (*slot).min(id);
        }
        for b in &ids.books {
            for isbn in [b.isbn_13.as_deref(), b.isbn_10.as_deref()]
                .into_iter()
                .flatten()
                .filter_map(thirteen)
            {
                if b.in_library {
                    in_library_isbns.insert(isbn.clone());
                }
                first(&mut by_isbn, isbn, b.id);
            }
            if let Some(k) = &b.openlibrary_key {
                let key = (ProviderId::OpenLibrary.as_str().to_string(), k.clone());
                first(&mut by_provider_id, key, b.id);
            }
            if let Some(g) = &b.googlebooks_id {
                let key = (ProviderId::GoogleBooks.as_str().to_string(), g.clone());
                first(&mut by_provider_id, key, b.id);
            }
            if let Some(p) = Prepared::new(&Query::new(b.title.as_deref(), &b.authors)) {
                titled.push((b.id, b.title.clone(), p));
            }
        }
        for (source, id, book) in ids.external {
            // Only an id a provider would carry is an identity here: a
            // Goodreads or calibre id names nothing a search returns.
            if ProviderId::parse(&source).is_some() {
                first(&mut by_provider_id, (source, id), book);
            }
        }
        let mut by_word: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, (_, _, p)) in titled.iter().enumerate() {
            for w in p.words() {
                by_word.entry(w.to_string()).or_default().push(i);
            }
        }
        Library {
            by_word,
            by_isbn: by_isbn.into_iter().collect(),
            by_provider_id: by_provider_id.into_iter().collect(),
            in_library_isbns,
            titled,
        }
    }

    /// Canonical ISBN-13s of the books **in the library** (the copies rule):
    /// after a typed ISBN, an edition the library holds is the one a work
    /// defaults to.
    pub(crate) fn in_library_isbns(&self) -> HashSet<String> {
        self.in_library_isbns.clone()
    }

    /// [`Held::Certain`]'s half: an edition's ISBN — the default edition's
    /// first — then a provider id.
    pub(crate) fn certain(&self, work: &WorkResult) -> Option<i64> {
        let default = work.editions.get(work.default_edition);
        let editions = default
            .into_iter()
            .chain(work.editions.iter())
            .map(|e| &e.book)
            .chain(std::iter::once(&work.flat));
        let books: Vec<_> = editions.collect();
        let by_isbn = books
            .iter()
            .filter_map(|b| b.canonical_isbn13())
            .find_map(|i| self.by_isbn.get(&i).copied());
        if by_isbn.is_some() {
            return by_isbn;
        }
        books.iter().find_map(|b| {
            let ol = b
                .openlibrary_key
                .as_ref()
                .map(|k| (ProviderId::OpenLibrary.as_str().to_string(), k.clone()));
            let gb = b
                .googlebooks_id
                .as_ref()
                .map(|g| (ProviderId::GoogleBooks.as_str().to_string(), g.clone()));
            [ol, gb]
                .into_iter()
                .flatten()
                .find_map(|k| self.by_provider_id.get(&k).copied())
        })
    }

    /// [`Held::Likely`]'s half: the held book the matcher links the work to
    /// with no question asked, under any title the work answers to (its own,
    /// and the other titles its provider gave: the matched edition's, the title
    /// with its subtitle). The best verdict wins; a tie, the lower id.
    pub(crate) fn likely(&self, work: &WorkResult) -> Option<(i64, Option<String>)> {
        let mut titles: Vec<&str> = work.title.iter().map(String::as_str).collect();
        for t in &work.facts.also_titled {
            if !titles.contains(&t.as_str()) {
                titles.push(t);
            }
        }
        let authors = &work.flat.authors;
        let mut best: Option<(f64, i64, &Option<String>)> = None;
        for t in titles {
            let Some(p) = Prepared::new(&Query::new(Some(t), authors)) else {
                continue;
            };
            let mut near: Vec<usize> = p
                .words()
                .filter_map(|w| self.by_word.get(w))
                .flatten()
                .copied()
                .collect();
            near.sort_unstable();
            near.dedup();
            for (id, title, held) in near.iter().map(|&i| &self.titled[i]) {
                let Some(v) = p.compare_prepared(held) else {
                    continue;
                };
                if !v.can_auto {
                    continue;
                }
                let better = match best {
                    None => true,
                    Some((s, bid, _)) => v.score > s || (v.score == s && *id < bid),
                };
                if better {
                    best = Some((v.score, *id, title));
                }
            }
        }
        best.map(|(_, id, title)| (id, title.clone()))
    }

    /// The whole answer for one work. The session asks the two halves itself
    /// (`session::held_of`), keeping the matcher's answers across arrivals.
    #[cfg(test)]
    pub(crate) fn held(&self, work: &WorkResult) -> Held {
        if let Some(id) = self.certain(work) {
            return Held::Certain(id);
        }
        match self.likely(work) {
            Some((book_id, title)) => Held::Likely { book_id, title },
            None => Held::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::Book;
    use crate::providers::{ProviderBook, ProviderWork};
    use crate::search::works::{DefaultContext, works};
    use crate::storage::LibraryBook;

    fn held_book(id: i64, title: &str, author: &str) -> LibraryBook {
        LibraryBook {
            id,
            title: Some(title.into()),
            authors: vec![author.into()],
            isbn_13: None,
            isbn_10: None,
            openlibrary_key: None,
            googlebooks_id: None,
            in_library: false,
        }
    }

    fn work(book: Book, also: &[&str]) -> WorkResult {
        let mut w = ProviderWork::bare(ProviderBook {
            book,
            provider: ProviderId::OpenLibrary,
            position: 0,
        });
        w.facts.also_titled = also.iter().map(|s| s.to_string()).collect();
        let ctx = DefaultContext {
            language: "en".into(),
            ..Default::default()
        };
        works(&[w], &ctx).remove(0)
    }

    fn found(title: &str, author: &str) -> Book {
        Book {
            title: Some(title.into()),
            authors: vec![author.into()],
            ..Default::default()
        }
    }

    #[test]
    fn an_identifier_is_certain_and_a_title_is_likely() {
        let mut middlemarch = held_book(1, "Middlemarch", "George Eliot");
        middlemarch.isbn_10 = Some("1900000172".into());
        let mut dune = held_book(2, "Dune", "Frank Herbert");
        dune.openlibrary_key = Some("/works/OL893415W".into());
        let messiah = held_book(3, "Dune Messiah", "Frank Herbert");
        let lib = Library::prepare(LibraryIdentities {
            books: vec![middlemarch, dune, messiah],
            external: vec![],
        });

        // The ISBN-13 of a held ISBN-10.
        let p = work(
            Book {
                isbn_13: Some("9781900000178".into()),
                ..found("Middlemarch: A Novel", "George Eliot")
            },
            &[],
        );
        assert_eq!(lib.held(&p), Held::Certain(1));
        // A provider id.
        let d = work(
            Book {
                openlibrary_key: Some("/works/OL893415W".into()),
                ..found("Dune (Deluxe)", "Frank Herbert")
            },
            &[],
        );
        assert_eq!(lib.held(&d), Held::Certain(2));
        // No identifier: the matcher's auto band.
        let m = work(found("Dune Messiah", "Frank Herbert"), &[]);
        assert_eq!(
            lib.held(&m),
            Held::Likely {
                book_id: 3,
                title: Some("Dune Messiah".into())
            }
        );
        // The band is not shown: Dune against Dune Messiah is a candidate,
        // never an answer.
        let lib = Library::prepare(LibraryIdentities {
            books: vec![held_book(3, "Dune Messiah", "Frank Herbert")],
            external: vec![],
        });
        assert_eq!(
            lib.held(&work(found("Dune", "Frank Herbert"), &[])),
            Held::None
        );
        // Another author with the same title is somebody else's book. (Brian
        // Herbert would not be: the matcher's veto is only a veto, and a shared
        // surname is agreement — `matching.rs` argues why.)
        assert_eq!(
            lib.held(&work(found("Dune Messiah", "Kevin J. Anderson"), &[])),
            Held::None
        );
    }

    /// A held title that is the work's *other* title — the one with its
    /// subtitle — is the work.
    #[test]
    fn any_title_the_work_answers_to_is_compared() {
        let lib = Library::prepare(LibraryIdentities {
            books: vec![held_book(
                9,
                "The Fellowship of the Ring: Being the First Part of The Lord of the Rings",
                "J.R.R. Tolkien",
            )],
            external: vec![],
        });
        let bare = work(found("The Fellowship of the Ring", "J. R. R. Tolkien"), &[]);
        assert_eq!(lib.held(&bare), Held::None, "a subtitle is the band");
        let subtitled = work(
            found("The Fellowship of the Ring", "J. R. R. Tolkien"),
            &["The Fellowship of the Ring: Being the First Part of the Lord of the Rings"],
        );
        assert_eq!(lib.held(&subtitled).book_id(), Some(9));
    }

    #[test]
    fn only_provider_external_ids_are_identities() {
        let lib = Library::prepare(LibraryIdentities {
            books: vec![held_book(4, "Something Else", "Nobody")],
            external: vec![
                ("goodreads".into(), "gb7".into(), 4),
                ("googlebooks".into(), "gb8".into(), 4),
            ],
        });
        let w = |id: &str| {
            work(
                Book {
                    googlebooks_id: Some(id.into()),
                    ..found("Emma", "Jane Austen")
                },
                &[],
            )
        };
        assert_eq!(lib.held(&w("gb7")), Held::None);
        assert_eq!(lib.held(&w("gb8")), Held::Certain(4));
    }

    #[test]
    fn only_books_in_the_library_lend_the_default_edition() {
        let mut kept = held_book(1, "A", "X");
        kept.isbn_13 = Some("9780441172719".into());
        kept.in_library = true;
        let mut listed = held_book(2, "B", "Y");
        listed.isbn_13 = Some("9780593099322".into());
        let lib = Library::prepare(LibraryIdentities {
            books: vec![kept, listed],
            external: vec![],
        });
        assert_eq!(
            lib.in_library_isbns(),
            HashSet::from(["9780441172719".to_string()])
        );
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::book::Book;
    use crate::providers::{ProviderBook, ProviderWork};
    use crate::search::works::{DefaultContext, works};
    use crate::storage::LibraryBook;
    use proptest::prelude::*;

    fn words() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just("the"),
                Just("dune"),
                Just("messiah"),
                Just("of"),
                Just("children"),
                Just("emma"),
                Just("dunes"),
                Just("a")
            ],
            1..4,
        )
        .prop_map(|w| w.join(" "))
    }

    proptest! {
        /// Comparing only the held books that share a word with the work
        /// finds exactly what comparing every held book finds: an auto match
        /// cannot share none (`Prepared::words`).
        #[test]
        fn the_shared_word_shortcut_changes_no_answer(
            held in proptest::collection::vec((words(), proptest::option::of(Just("Frank Herbert"))), 0..8),
            title in words(),
        ) {
            let books: Vec<LibraryBook> = held
                .iter()
                .enumerate()
                .map(|(i, (t, a))| LibraryBook {
                    id: i as i64 + 1,
                    title: Some(t.clone()),
                    authors: a.iter().map(|s| s.to_string()).collect(),
                    isbn_13: None,
                    isbn_10: None,
                    openlibrary_key: None,
                    googlebooks_id: None,
                    in_library: true,
                })
                .collect();
            let lib = Library::prepare(LibraryIdentities { books, external: vec![] });
            let w = works(
                &[ProviderWork::bare(ProviderBook {
                    book: Book {
                        title: Some(title.clone()),
                        authors: vec!["Frank Herbert".into()],
                        ..Default::default()
                    },
                    provider: ProviderId::OpenLibrary,
                    position: 0,
                })],
                &DefaultContext { language: "en".into(), ..Default::default() },
            )
            .remove(0);
            let brute = Prepared::new(&Query::new(Some(&title), &w.flat.authors)).and_then(|p| {
                let mut best: Option<(f64, i64)> = None;
                for (id, _, h) in &lib.titled {
                    if let Some(v) = p.compare_prepared(h).filter(|v| v.can_auto)
                        && best.is_none_or(|(s, b)| v.score > s || (v.score == s && *id < b))
                    {
                        best = Some((v.score, *id));
                    }
                }
                best.map(|(_, id)| id)
            });
            prop_assert_eq!(lib.likely(&w).map(|(id, _)| id), brute);
        }
    }
}
