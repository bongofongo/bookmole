//! "You have this": matches a search result to held books, `Certain` by identifier, `Likely` by
//! the shared matcher. The library is read and prepared once per search, never once per row.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::works::WorkResult;
use crate::matching::{Prepared, Query};
use crate::providers::ProviderId;
use crate::storage::LibraryIdentities;

#[derive(Debug, Clone, Default, PartialEq)]
pub enum Held {
    #[default]
    None,
    Certain(i64),
    Likely { book_id: i64, title: Option<String> },
}

impl Held {
    pub fn book_id(&self) -> Option<i64> {
        match self {
            Held::None => None,
            Held::Certain(id) | Held::Likely { book_id: id, .. } => Some(*id),
        }
    }

    /// Only an identifier match, which is what may open the book directly.
    pub fn certain(&self) -> Option<i64> {
        match self {
            Held::Certain(id) => Some(*id),
            _ => None,
        }
    }
}

fn thirteen(isbn: &str) -> Option<String> {
    if isbn.len() == 13 {
        Some(isbn.to_string())
    } else {
        crate::book::isbn10_to_13(isbn)
    }
}

pub(crate) struct Library {
    by_isbn: HashMap<String, i64>,
    by_provider_id: HashMap<(String, String), i64>,
    in_library_isbns: HashSet<String>,
    titled: Vec<(i64, Option<String>, Prepared)>,
    /// Content word -> `titled` entries with it; an auto match shares a word, so only these compare.
    by_word: HashMap<String, Vec<usize>>,
}

impl Library {
    /// When two books claim one identifier the lower id answers, so one library answers one way.
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
            // Only an id a provider carries is an identity; a Goodreads or calibre id matches no search result.
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

    pub(crate) fn in_library_isbns(&self) -> HashSet<String> {
        self.in_library_isbns.clone()
    }

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

    /// The best verdict across all the work's titles wins; a tie goes to the lower id.
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

        let p = work(
            Book {
                isbn_13: Some("9781900000178".into()),
                ..found("Middlemarch: A Novel", "George Eliot")
            },
            &[],
        );
        assert_eq!(lib.held(&p), Held::Certain(1));
        let d = work(
            Book {
                openlibrary_key: Some("/works/OL893415W".into()),
                ..found("Dune (Deluxe)", "Frank Herbert")
            },
            &[],
        );
        assert_eq!(lib.held(&d), Held::Certain(2));
        let m = work(found("Dune Messiah", "Frank Herbert"), &[]);
        assert_eq!(
            lib.held(&m),
            Held::Likely {
                book_id: 3,
                title: Some("Dune Messiah".into())
            }
        );
        let lib = Library::prepare(LibraryIdentities {
            books: vec![held_book(3, "Dune Messiah", "Frank Herbert")],
            external: vec![],
        });
        assert_eq!(
            lib.held(&work(found("Dune", "Frank Herbert"), &[])),
            Held::None
        );
        assert_eq!(
            lib.held(&work(found("Dune Messiah", "Kevin J. Anderson"), &[])),
            Held::None
        );
    }

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
