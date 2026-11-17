//! Works and their editions: a search result is a work with its providers' editions inside it.
//! Grouping is independent of the order the providers answered in: records are processed in a
//! canonical order. The default edition is chosen here, never by a frontend.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::held::Held;
use super::{FieldClaims, fingerprint, merge_facts, merge_into, same_work};
use crate::book::Book;
use crate::providers::{ProviderId, ProviderWork, WorkFacts};

#[derive(Debug, Clone)]
pub struct Edition {
    pub edition: String,
    pub book: Book,
    pub sources: Vec<ProviderId>,
    pub(crate) claims: FieldClaims,
    /// A provider matched this edition to the query; a strong hint when choosing the default edition.
    pub(crate) matched: bool,
}

#[derive(Debug, Clone)]
pub struct WorkResult {
    /// Identity within a search, from its earliest-arrived record.
    pub(crate) key: String,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub first_published: Option<i64>,
    pub original_title: Option<String>,
    pub series: Option<String>,
    pub series_position: Option<f64>,
    pub cover_url: Option<String>,
    pub languages: Vec<String>,
    pub edition_count: Option<u32>,
    pub editions: Vec<Edition>,
    pub default_edition: usize,
    pub sources: Vec<ProviderId>,
    pub score: f64,
    /// What the app holds of this work: set by the session once the library is read; [`Held::None`]
    /// before that.
    pub held: Held,
    pub(crate) best_position: usize,
    pub(crate) facts: WorkFacts,
    /// The default edition filled from the work's other records with everything not specific to one
    /// edition. What `Engine::search` returns and ranking reads.
    pub(crate) flat: Book,
    pub(crate) flat_claims: FieldClaims,
}

impl WorkResult {
    pub fn default(&self) -> &Edition {
        &self.editions[self.default_edition]
    }

    pub fn flattened(&self) -> &Book {
        &self.flat
    }

    /// Who supplied each field of [`WorkResult::flattened`].
    pub fn flattened_claims(&self) -> Vec<super::FieldClaim> {
        self.flat_claims.to_vec()
    }

    pub(crate) fn has_isbn(&self, isbn: &str) -> bool {
        self.editions
            .iter()
            .any(|e| e.book.canonical_isbn13().as_deref() == Some(isbn))
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DefaultContext {
    pub typed_isbn: Option<String>,
    /// Canonical ISBN-13s of books in the library by the copies rule: a book with no copy lends no
    /// default edition.
    pub held_isbns: HashSet<String>,
    /// The query's `lang:`, else the library's commonest language, else `en`.
    pub language: String,
}

fn identity(seq: usize, w: &ProviderWork) -> String {
    let p = w.found.provider;
    if let Some(k) = w.work_key() {
        return format!("{p}:{k}");
    }
    if let Some(i) = w.found.book.canonical_isbn13() {
        return format!("{p}:isbn:{i}");
    }
    if let Some(g) = &w.found.book.googlebooks_id {
        return format!("{p}:gb:{g}");
    }
    format!("{p}#{seq}")
}

/// Never by arrival: the same answers must group the same way whichever came first.
fn canonical_key(w: &ProviderWork) -> (ProviderId, String, String) {
    (
        w.found.provider,
        w.work_key().unwrap_or_default().to_string(),
        format!("{:?}", w.found.book),
    )
}

struct Groups {
    parent: Vec<usize>,
    works: Vec<BTreeMap<ProviderId, BTreeSet<String>>>,
}

impl Groups {
    fn new(records: &[ProviderWork]) -> Self {
        Groups {
            parent: (0..records.len()).collect(),
            works: records
                .iter()
                .map(|w| {
                    let mut m = BTreeMap::new();
                    if let Some(k) = w.work_key() {
                        m.insert(w.found.provider, BTreeSet::from([k.to_string()]));
                    }
                    m
                })
                .collect(),
        }
    }

    fn root(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    fn contradict(&mut self, a: usize, b: usize) -> bool {
        let (a, b) = (self.root(a), self.root(b));
        self.works[a]
            .iter()
            .any(|(p, ka)| self.works[b].get(p).is_some_and(|kb| ka != kb))
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.root(a), self.root(b));
        if a == b {
            return;
        }
        let (keep, gone) = if a < b { (a, b) } else { (b, a) };
        self.parent[gone] = keep;
        let moved = std::mem::take(&mut self.works[gone]);
        for (p, ks) in moved {
            self.works[keep].entry(p).or_default().extend(ks);
        }
    }
}

pub(crate) fn group(records: &[ProviderWork]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..records.len()).collect();
    order.sort_by_cached_key(|&i| canonical_key(&records[i]));
    let mut g = Groups::new(records);

    let mut by_work: HashMap<(ProviderId, &str), usize> = HashMap::new();
    for &i in &order {
        if let Some(k) = records[i].work_key() {
            match by_work.get(&(records[i].found.provider, k)) {
                Some(&j) => g.union(i, j),
                None => {
                    by_work.insert((records[i].found.provider, k), i);
                }
            }
        }
    }
    let mut by_isbn: HashMap<String, usize> = HashMap::new();
    for &i in &order {
        if let Some(isbn) = records[i].found.book.canonical_isbn13() {
            match by_isbn.get(&isbn) {
                Some(&j) => g.union(i, j),
                None => {
                    by_isbn.insert(isbn, i);
                }
            }
        }
    }
    let prints: Vec<String> = records.iter().map(|w| fingerprint(&w.found.book)).collect();
    for (a, &i) in order.iter().enumerate() {
        for &j in &order[a + 1..] {
            if prints[i] == prints[j]
                && same_work(&records[i].found.book, &records[j].found.book)
                && !g.contradict(i, j)
            {
                g.union(i, j);
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for &i in &order {
        let r = g.root(i);
        groups.entry(r).or_default().push(i);
    }
    let mut out: Vec<Vec<usize>> = groups.into_values().collect();
    out.sort_by_key(|members| members.iter().copied().min());
    out
}

fn without_edition_fields(book: &Book) -> Book {
    Book {
        isbn_10: None,
        isbn_13: None,
        publisher: None,
        publish_year: None,
        page_count: None,
        language: None,
        googlebooks_id: None,
        ..book.clone()
    }
}

pub(crate) fn works(records: &[ProviderWork], ctx: &DefaultContext) -> Vec<WorkResult> {
    group(records)
        .into_iter()
        .map(|members| work_of(records, &members, ctx))
        .collect()
}

fn work_of(records: &[ProviderWork], members: &[usize], ctx: &DefaultContext) -> WorkResult {
    let first = *members.iter().min().expect("a group has a member");
    let key = identity(first, &records[first]);

    let mut editions: Vec<Edition> = Vec::new();
    let mut by_isbn: HashMap<String, usize> = HashMap::new();
    let mut sources: Vec<ProviderId> = Vec::new();
    let mut facts = WorkFacts::default();
    let mut best_position = usize::MAX;
    for &i in members {
        let w = &records[i];
        let p = w.found.provider;
        if !sources.contains(&p) {
            sources.push(p);
        }
        best_position = best_position.min(w.found.position);
        merge_facts(&mut facts, w.facts.clone());
        let matched = p == ProviderId::OpenLibrary;
        let isbn = w.found.book.canonical_isbn13();
        match isbn.as_ref().and_then(|k| by_isbn.get(k).copied()) {
            Some(e) => {
                let ed = &mut editions[e];
                merge_into(&mut ed.book, &w.found.book, p, &mut ed.claims);
                if !ed.sources.contains(&p) {
                    ed.sources.push(p);
                }
                ed.matched |= matched;
            }
            None => {
                let mut claims = FieldClaims::default();
                for field in crate::storage::fields_said(&w.found.book) {
                    claims.claim(field, p);
                }
                let handle = match &isbn {
                    Some(k) => format!("isbn:{k}"),
                    None => identity(i, w),
                };
                if let Some(k) = isbn {
                    by_isbn.insert(k, editions.len());
                }
                editions.push(Edition {
                    edition: handle,
                    book: w.found.book.clone(),
                    sources: vec![p],
                    claims,
                    matched,
                });
            }
        }
    }
    let default_edition = default_edition(&editions, ctx);

    let mut flat = editions[default_edition].book.clone();
    let mut flat_claims = editions[default_edition].claims.clone();
    let default_isbn = flat.canonical_isbn13();
    for &i in members {
        let w = &records[i];
        let same_edition =
            default_isbn.is_some() && w.found.book.canonical_isbn13() == default_isbn;
        if !same_edition {
            merge_into(
                &mut flat,
                &without_edition_fields(&w.found.book),
                w.found.provider,
                &mut flat_claims,
            );
        }
    }

    let first_published = members
        .iter()
        .map(|&i| &records[i])
        .find(|w| {
            w.found.provider == ProviderId::OpenLibrary && w.found.book.first_published.is_some()
        })
        .or_else(|| {
            members
                .iter()
                .map(|&i| &records[i])
                .find(|w| w.found.book.first_published.is_some())
        })
        .and_then(|w| w.found.book.first_published);
    let (series, series_position) = match &facts.series {
        Some(s) => (Some(s.clone()), facts.series_position),
        None => (flat.series.clone(), flat.series_index),
    };
    let mut languages = facts.languages.clone();
    for e in &editions {
        if let Some(l) = &e.book.language
            && !languages.contains(l)
        {
            languages.push(l.clone());
        }
    }
    WorkResult {
        key,
        title: flat.title.clone(),
        authors: flat.authors_in_display_order(),
        first_published,
        original_title: None,
        series,
        series_position,
        cover_url: editions[default_edition]
            .book
            .cover_url
            .clone()
            .or_else(|| flat.cover_url.clone()),
        languages,
        edition_count: facts.edition_count,
        default_edition,
        editions,
        sources,
        score: 0.0,
        held: Held::None,
        best_position,
        facts,
        flat,
        flat_claims,
    }
}

pub(crate) fn default_edition(editions: &[Edition], ctx: &DefaultContext) -> usize {
    let rank = |e: &Edition| {
        let isbn = e.book.canonical_isbn13();
        (
            // Each is a "better" flag; `max_by_key` keeps the last of equals, so the index is negated to keep
            // the first.
            ctx.typed_isbn.is_some() && isbn == ctx.typed_isbn,
            isbn.as_ref().is_some_and(|i| ctx.held_isbns.contains(i)),
            e.matched,
            e.book.language.as_deref() == Some(ctx.language.as_str()),
            isbn.is_some(),
            e.book.cover_url.is_some(),
            e.book.publish_year.unwrap_or(i64::MIN),
        )
    };
    editions
        .iter()
        .enumerate()
        .max_by_key(|(i, e)| (rank(e), std::cmp::Reverse(*i)))
        .map_or(0, |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderBook;

    fn rec(provider: ProviderId, position: usize, book: Book) -> ProviderWork {
        ProviderWork::bare(ProviderBook {
            book,
            provider,
            position,
        })
    }

    fn book(title: &str, author: &str) -> Book {
        Book {
            title: Some(title.into()),
            authors: vec![author.into()],
            ..Default::default()
        }
    }

    fn ol(title: &str, author: &str, work: &str, isbn: Option<&str>) -> Book {
        Book {
            openlibrary_key: Some(work.into()),
            isbn_13: isbn.map(str::to_string),
            ..book(title, author)
        }
    }

    fn ctx() -> DefaultContext {
        DefaultContext {
            language: "en".into(),
            ..Default::default()
        }
    }

    #[test]
    fn volumes_join_a_work_and_stay_its_editions() {
        let records = vec![
            rec(
                ProviderId::OpenLibrary,
                0,
                ol("Dune", "Frank Herbert", "/works/W1", Some("9780441172719")),
            ),
            rec(
                ProviderId::GoogleBooks,
                0,
                Book {
                    isbn_13: Some("9780441172719".into()),
                    description: Some("Arrakis.".into()),
                    ..book("Dune", "Frank Herbert")
                },
            ),
            rec(
                ProviderId::GoogleBooks,
                1,
                Book {
                    isbn_13: Some("9780593099322".into()),
                    publisher: Some("Ace".into()),
                    ..book("Dune", "Frank Herbert")
                },
            ),
            rec(
                ProviderId::GoogleBooks,
                2,
                book("Dune Messiah", "Frank Herbert"),
            ),
        ];
        let ws = works(&records, &ctx());
        assert_eq!(ws.len(), 2, "{ws:#?}");
        let dune = &ws[0];
        assert_eq!(dune.editions.len(), 2, "two ISBNs, two editions");
        assert_eq!(
            dune.sources,
            vec![ProviderId::OpenLibrary, ProviderId::GoogleBooks]
        );
        assert_eq!(
            dune.default().book.isbn_13.as_deref(),
            Some("9780441172719"),
            "Open Library's matched edition"
        );
        assert_eq!(dune.flat.description.as_deref(), Some("Arrakis."));
        assert_eq!(dune.flat.publisher, None);
        assert_eq!(dune.flat.openlibrary_key.as_deref(), Some("/works/W1"));
        assert_eq!(dune.key, "openlibrary:/works/W1");
    }

    #[test]
    fn two_work_ids_of_one_provider_are_never_merged_by_title() {
        let records = vec![
            rec(
                ProviderId::OpenLibrary,
                0,
                ol("Dune", "Frank Herbert", "/works/W1", None),
            ),
            rec(
                ProviderId::OpenLibrary,
                1,
                ol("Dune", "Frank Herbert", "/works/W2", None),
            ),
            rec(ProviderId::GoogleBooks, 0, book("Dune", "Frank Herbert")),
        ];
        let groups = group(&records);
        assert_eq!(groups.len(), 2, "{groups:?}");
    }

    #[test]
    fn the_default_edition_follows_the_spec_order() {
        let ed = |isbn: Option<&str>, lang: Option<&str>, matched: bool, cover: bool, year: i64| {
            Edition {
                edition: String::new(),
                book: Book {
                    isbn_13: isbn.map(str::to_string),
                    language: lang.map(str::to_string),
                    cover_url: cover.then(|| "https://x/c.jpg".to_string()),
                    publish_year: Some(year),
                    ..Default::default()
                },
                sources: vec![],
                claims: FieldClaims::default(),
                matched,
            }
        };
        let eds = vec![
            ed(Some("9780000000002"), Some("fr"), false, true, 2020),
            ed(Some("9780441172719"), Some("en"), true, false, 1990),
            ed(None, Some("en"), false, true, 2024),
            ed(Some("9780593099322"), Some("en"), false, true, 2019),
        ];
        assert_eq!(default_edition(&eds, &ctx()), 1);
        let typed = DefaultContext {
            typed_isbn: Some("9780000000002".into()),
            ..ctx()
        };
        assert_eq!(default_edition(&eds, &typed), 0);
        let held = DefaultContext {
            held_isbns: HashSet::from(["9780593099322".to_string()]),
            ..ctx()
        };
        assert_eq!(default_edition(&eds, &held), 3);
        let unmatched: Vec<Edition> = eds
            .iter()
            .cloned()
            .map(|mut e| {
                e.matched = false;
                e
            })
            .collect();
        assert_eq!(default_edition(&unmatched, &ctx()), 3);
        let fr = DefaultContext {
            language: "fr".into(),
            ..ctx()
        };
        assert_eq!(default_edition(&unmatched, &fr), 0);
        assert_eq!(default_edition(&[], &ctx()), 0);
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::providers::ProviderBook;
    use proptest::prelude::*;

    fn arb_record() -> impl Strategy<Value = ProviderWork> {
        (
            prop_oneof![Just(ProviderId::OpenLibrary), Just(ProviderId::GoogleBooks)],
            prop_oneof![
                Just("Dune"),
                Just("Dune Messiah"),
                Just("The Dune"),
                Just("Emma")
            ],
            prop_oneof![
                Just("Frank Herbert"),
                Just("Brian Herbert"),
                Just("Jane Austen")
            ],
            proptest::option::of(prop_oneof![Just("/works/W1"), Just("/works/W2")]),
            proptest::option::of(prop_oneof![
                Just("9780441172719"),
                Just("9780593099322"),
                Just("9780141439587")
            ]),
            0usize..20,
        )
            .prop_map(|(provider, title, author, work, isbn, position)| {
                let book = Book {
                    title: Some(title.into()),
                    authors: vec![author.into()],
                    openlibrary_key: work
                        .filter(|_| provider == ProviderId::OpenLibrary)
                        .map(str::to_string),
                    isbn_13: isbn.map(str::to_string),
                    ..Default::default()
                };
                ProviderWork::bare(ProviderBook {
                    book,
                    provider,
                    position,
                })
            })
    }

    fn shape(records: &[ProviderWork], groups: &[Vec<usize>]) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = groups
            .iter()
            .map(|g| {
                let mut keys: Vec<String> = g
                    .iter()
                    .map(|&i| format!("{:?}", canonical_key(&records[i])))
                    .collect();
                keys.sort();
                keys
            })
            .collect();
        out.sort();
        out
    }

    proptest! {
        #[test]
        fn grouping_is_independent_of_arrival_order(
            records in proptest::collection::vec(arb_record(), 0..10),
            seed in any::<u64>(),
        ) {
            let mut shuffled = records.clone();
            let n = shuffled.len();
            let mut s = seed;
            for i in (1..n).rev() {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                shuffled.swap(i, (s >> 33) as usize % (i + 1));
            }
            prop_assert_eq!(
                shape(&records, &group(&records)),
                shape(&shuffled, &group(&shuffled))
            );
        }

        #[test]
        fn grouping_partitions_and_keeps_work_ids_apart(
            records in proptest::collection::vec(arb_record(), 0..10),
        ) {
            let groups = group(&records);
            let mut seen: Vec<usize> = groups.iter().flatten().copied().collect();
            seen.sort_unstable();
            prop_assert_eq!(seen, (0..records.len()).collect::<Vec<_>>());
            for g in &groups {
                let ids: BTreeSet<&str> = g.iter().filter_map(|&i| records[i].work_key()).collect();
                if ids.len() > 1 {
                    let isbns: Vec<_> = g.iter().filter_map(|&i| records[i].found.book.canonical_isbn13()).collect();
                    prop_assert!(!isbns.is_empty(), "{:?}", ids);
                }
            }
        }

        #[test]
        fn the_default_edition_is_one_of_the_editions(
            records in proptest::collection::vec(arb_record(), 1..10),
            typed in proptest::option::of(prop_oneof![Just("9780441172719"), Just("9780000000002")]),
        ) {
            let ctx = DefaultContext {
                typed_isbn: typed.map(str::to_string),
                language: "en".into(),
                ..Default::default()
            };
            for w in works(&records, &ctx) {
                prop_assert!(w.default_edition < w.editions.len());
                if let Some(t) = &ctx.typed_isbn && w.has_isbn(t) {
                    let got = w.default().book.canonical_isbn13();
                    prop_assert_eq!(got.as_deref(), Some(t.as_str()));
                }
            }
        }
    }
}
