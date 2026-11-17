//! Works and their editions.
//!
//! A search result is a **work**, with the editions the providers returned
//! inside it: one row per book, the editions opening as a chooser, rather than
//! a row per edition that buries the book under its own printings. Providers
//! answer in their own shapes — Open Library in works with
//! one matched edition each, Google Books in volumes, which are editions with
//! no work above them — and this module puts them into one shape:
//!
//! 1. **Grouping** ([`group`]), strongest evidence first: two records naming the same
//!    provider work id are one work; two records sharing a canonical ISBN-13
//!    are one work; otherwise the existing dedup fingerprint (`search::same_work`
//!    — no second "same book" matcher), and only between records whose work ids
//!    do not contradict each other. Two Open Library works with the same title
//!    and author are two works: Open Library said so, and it holds both.
//! 2. **Editions** inside a work are deduplicated on canonical ISBN-13 and
//!    merged field by field by `MERGE_RULES` (the one table, no second dialect).
//! 3. **The default edition** ([`default_edition`]) is the engine's choice, never
//!    a frontend's: the edition whose ISBN was typed; one the library holds;
//!    the provider's own best match; the preferred language; having an ISBN;
//!    having a jacket; the most recent.
//!
//! Grouping is **independent of the order the providers answered in** (a
//! property below): records are processed in a canonical order, not arrival
//! order, so the same answers group the same way whichever came first.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::held::Held;
use super::{FieldClaims, fingerprint, merge_facts, merge_into, same_work};
use crate::book::Book;
use crate::providers::{ProviderId, ProviderWork, WorkFacts};

/// One edition of a work, as the providers that returned it describe it.
#[derive(Debug, Clone)]
pub struct Edition {
    /// Opaque, stable within a search: the canonical ISBN-13 where there is one.
    pub edition: String,
    /// The edition's fields, merged across the providers that returned it.
    pub book: Book,
    pub sources: Vec<ProviderId>,
    pub(crate) claims: FieldClaims,
    /// A provider matched this edition to the query (Open Library's nested
    /// best edition, or an ISBN lookup). A strong hint when choosing the
    /// default edition.
    pub(crate) matched: bool,
}

/// One work: the unit of a search result.
#[derive(Debug, Clone)]
pub struct WorkResult {
    /// Identity within a search, from its earliest-arrived record; the session
    /// turns it into a row handle.
    pub(crate) key: String,
    pub title: Option<String>,
    /// Each written the way a person says it (`names.rs`).
    pub authors: Vec<String>,
    pub first_published: Option<i64>,
    /// The work's title in its original language. `None` until a provider
    /// that knows it (such as Wikidata) is built.
    pub original_title: Option<String>,
    pub series: Option<String>,
    pub series_position: Option<f64>,
    pub cover_url: Option<String>,
    /// Languages its editions come in, two-letter where known.
    pub languages: Vec<String>,
    /// How many editions the provider holds, when it says.
    pub edition_count: Option<u32>,
    /// The editions returned so far.
    pub editions: Vec<Edition>,
    /// Index into `editions`.
    pub default_edition: usize,
    pub sources: Vec<ProviderId>,
    pub score: f64,
    /// What the app holds of this work (`held.rs`): set by the session once
    /// the library is read, [`Held::None`] before that and in a search with no
    /// library to read.
    pub held: Held,
    pub(crate) best_position: usize,
    pub(crate) facts: WorkFacts,
    /// The work as one record: its default edition, filled from the work's
    /// other records with everything that is not a fact about one edition.
    /// What `Engine::search` returns and what ranking reads.
    pub(crate) flat: Book,
    /// Who supplied each field of `flat` — what adding a search result with
    /// provenance would read; nothing reads it yet.
    #[allow(dead_code)]
    pub(crate) flat_claims: FieldClaims,
}

impl WorkResult {
    /// The default edition.
    pub fn default(&self) -> &Edition {
        &self.editions[self.default_edition]
    }

    /// The work as one `Book` — the shape `Engine::search` has always
    /// returned: the default edition, filled from the work's other records.
    pub fn flattened(&self) -> &Book {
        &self.flat
    }

    /// Whether `isbn` (canonical ISBN-13) is one of this work's editions.
    pub(crate) fn has_isbn(&self, isbn: &str) -> bool {
        self.editions
            .iter()
            .any(|e| e.book.canonical_isbn13().as_deref() == Some(isbn))
    }
}

/// What the default edition is chosen against.
#[derive(Debug, Clone, Default)]
pub(crate) struct DefaultContext {
    /// The typed ISBN, canonical ISBN-13.
    pub typed_isbn: Option<String>,
    /// Canonical ISBN-13s of the books **in the library** by the copies rule
    /// (`search::held::Library::in_library_isbns`). A book the app knows but
    /// holds no copy of (only on a list, only from Goodreads) lends no default:
    /// "an edition the library already holds" is the library's, not the
    /// app's.
    pub held_isbns: HashSet<String>,
    /// The query's `lang:`, else the library's commonest language, else `en`.
    pub language: String,
}

/// A record's identity, from what it says about itself.
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

/// The order records are processed in: by provider, then by what each says,
/// never by when it arrived.
fn canonical_key(w: &ProviderWork) -> (ProviderId, String, String) {
    (
        w.found.provider,
        w.work_key().unwrap_or_default().to_string(),
        format!("{:?}", w.found.book),
    )
}

struct Groups {
    parent: Vec<usize>,
    /// Each root's provider work ids, by provider.
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

    /// Whether the two groups name different works of one provider.
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

/// Partition `records` into works, each a list of indices into `records` in
/// canonical order. The groups come back ordered by their first member's
/// index, which in a session is arrival order.
pub(crate) fn group(records: &[ProviderWork]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..records.len()).collect();
    order.sort_by_cached_key(|&i| canonical_key(&records[i]));
    let mut g = Groups::new(records);

    // 1. A provider's own work id.
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
    // 2. A shared ISBN between editions.
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
    // 3. (Cross-links between providers — Open Library records carrying
    //    Wikidata ids, say — would go here once a provider that supplies them
    //    is built.) 4. Title and first author, where no work id contradicts.
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

/// Fields that describe **one edition** and never fill another's.
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

/// Build the works out of `records`, grouped by [`group`], each with its
/// default edition chosen and its flat record made. Unscored.
pub(crate) fn works(records: &[ProviderWork], ctx: &DefaultContext) -> Vec<WorkResult> {
    group(records)
        .into_iter()
        .map(|members| work_of(records, &members, ctx))
        .collect()
}

fn work_of(records: &[ProviderWork], members: &[usize], ctx: &DefaultContext) -> WorkResult {
    let first = *members.iter().min().expect("a group has a member");
    let key = identity(first, &records[first]);

    // Editions, deduplicated on canonical ISBN-13.
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
        // Open Library's book is its matched edition (or, by ISBN, the edition
        // itself); a volume is an edition nobody matched for us.
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

    // The flat record: the default edition, then every other record's
    // work-level fields, in canonical order.
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

/// The default edition, as an index into `editions`. Never out of
/// range for a non-empty list (a property).
pub(crate) fn default_edition(editions: &[Edition], ctx: &DefaultContext) -> usize {
    let rank = |e: &Edition| {
        let isbn = e.book.canonical_isbn13();
        (
            // Each a "better" flag; `max_by_key` keeps the last of equals, so
            // the index is negated to keep the first.
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

    /// Google's volumes of one work join Open Library's work by ISBN and by
    /// title, and stay editions inside it.
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
        // The flat record is the default edition, filled with the work-level
        // description — and not with the other edition's publisher.
        assert_eq!(dune.flat.description.as_deref(), Some("Arrakis."));
        assert_eq!(dune.flat.publisher, None);
        assert_eq!(dune.flat.openlibrary_key.as_deref(), Some("/works/W1"));
        assert_eq!(dune.key, "openlibrary:/works/W1");
    }

    /// Two Open Library works are two works, however alike they read.
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
            // …and a title-only volume between them joins one, not both.
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
        // The provider's match first, failing anything stronger.
        assert_eq!(default_edition(&eds, &ctx()), 1);
        // The typed ISBN beats everything.
        let typed = DefaultContext {
            typed_isbn: Some("9780000000002".into()),
            ..ctx()
        };
        assert_eq!(default_edition(&eds, &typed), 0);
        // A held edition beats the provider's match.
        let held = DefaultContext {
            held_isbns: HashSet::from(["9780593099322".to_string()]),
            ..ctx()
        };
        assert_eq!(default_edition(&eds, &held), 3);
        // With no match: language, then an ISBN, then a jacket, then recency.
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

    /// A partition as what it holds, not where: each group's members by their
    /// canonical key, sorted.
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
        /// Grouping is independent of the order the providers answered in.
        #[test]
        fn grouping_is_independent_of_arrival_order(
            records in proptest::collection::vec(arb_record(), 0..10),
            seed in any::<u64>(),
        ) {
            let mut shuffled = records.clone();
            // A deterministic shuffle from the seed.
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

        /// Every record is in exactly one work, and two Open Library work ids
        /// never share one through a title.
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
                // More than one id in a group is only ever an ISBN's doing.
                if ids.len() > 1 {
                    let isbns: Vec<_> = g.iter().filter_map(|&i| records[i].found.book.canonical_isbn13()).collect();
                    prop_assert!(!isbns.is_empty(), "{:?}", ids);
                }
            }
        }

        /// The default edition is among the editions.
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
