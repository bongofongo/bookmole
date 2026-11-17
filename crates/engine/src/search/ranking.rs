//! Scores how well a result answers a `ParsedQuery`; `rank` is pure, weights come from the bench.
//! An exact ISBN outweighs every other signal at its ceiling; standing never beats text.

use std::collections::BTreeSet;

use strsim::jaro_winkler;

use super::query::{ParsedQuery, Reading, fold, is_filler, title_key};
use crate::book::{Book, isbn10_to_13};
use crate::matching::title_similarity;
use crate::providers::{ProviderId, WorkFacts};

#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub book: &'a Book,
    pub facts: &'a WorkFacts,
    pub sources: &'a [ProviderId],
    /// The lowest position any provider gave it, from 0.
    pub best_position: usize,
    pub notability: Option<u32>,
}

pub const IDENTITY: f64 = 1000.0;
pub const TEXT: f64 = 100.0;
pub const STANDING: f64 = 14.0;
pub const NOTABILITY: f64 = 6.0;
pub const AGREEMENT: f64 = 3.0;
pub const POSITION: f64 = 4.0;
pub const LANGUAGE: f64 = 1.0;

/// Everything but identity at its ceiling; `IDENTITY` must exceed it.
pub const MAX_WITHOUT_IDENTITY: f64 =
    TEXT + STANDING + NOTABILITY + AGREEMENT + POSITION + LANGUAGE;

const STANDING_FULL: f64 = 1000.0;

const NOTABILITY_FULL: f64 = 150.0;

const DEFAULT_LANGUAGE: &str = "en";

// The score is the weighted mean over the parts a reading has, so title-only and
// title-and-author readings are both `0..=1`.
const PART_TITLE: f64 = 0.5;
const PART_AUTHOR: f64 = 0.35;
const PART_SERIES: f64 = 0.3;
const PART_POSITION: f64 = 0.3;
const PART_COVERAGE: f64 = 0.2;

const NEAR_WORD: f64 = 0.9;
const NEAR_CREDIT: f64 = 0.7;

pub fn rank(q: &ParsedQuery, c: &Candidate<'_>) -> f64 {
    let side = Side::of(c);
    let mut score = 0.0;

    if let Some(want) = &q.isbn
        && side.has_isbn(c.book, want)
    {
        score += IDENTITY;
    }

    score += TEXT * text_of(q, &side);

    if let Some(n) = c.facts.edition_count {
        score += STANDING * standing(n);
    }
    if let Some(n) = c.notability {
        score += NOTABILITY * notability(n);
    }
    if c.sources.len() > 1 {
        score += AGREEMENT;
    }
    score += POSITION / (1.0 + c.best_position as f64);

    let lang = q.lang.as_deref().unwrap_or(DEFAULT_LANGUAGE);
    if c.facts.languages.iter().any(|l| l == lang) || c.book.language.as_deref() == Some(lang) {
        score += LANGUAGE;
    }
    score
}

pub(crate) fn text_part(q: &ParsedQuery, c: &Candidate<'_>) -> f64 {
    text_of(q, &Side::of(c))
}

fn text_of(q: &ParsedQuery, side: &Side) -> f64 {
    q.readings
        .iter()
        .map(|r| r.weight * reading_score(q, r, side))
        .fold(0.0_f64, f64::max)
}

fn notability(sitelinks: u32) -> f64 {
    ((1.0 + f64::from(sitelinks)).ln() / (1.0 + NOTABILITY_FULL).ln()).min(1.0)
}

fn standing(editions: u32) -> f64 {
    ((1.0 + f64::from(editions)).ln() / (1.0 + STANDING_FULL).ln()).min(1.0)
}

fn reading_score(q: &ParsedQuery, r: &Reading, side: &Side) -> f64 {
    let mut num = 0.0;
    let mut den = 0.0;
    if let Some(t) = &r.title {
        num += PART_TITLE * side.title(t);
        den += PART_TITLE;
    }
    if let Some(a) = &r.author {
        num += PART_AUTHOR * side.author(a);
        den += PART_AUTHOR;
    }
    if let Some(s) = &r.series {
        num += PART_SERIES * side.series(s);
        den += PART_SERIES;
        if let Some(p) = r.position {
            let same = side
                .series_position
                .is_some_and(|have| (have - p).abs() < 1e-9);
            num += PART_POSITION * f64::from(u8::from(same));
            den += PART_POSITION;
        }
    }
    if den == 0.0 {
        return 0.0;
    }
    // A series reading's `book 2` is a position scored above, not a word the result must carry.
    let words: Vec<String> = [&r.title, &r.author, &r.series]
        .into_iter()
        .flatten()
        .flat_map(|part| part.split_whitespace().map(str::to_string))
        .collect();
    let words = if words.is_empty() { &q.words } else { &words };
    num += PART_COVERAGE * side.coverage(words);
    den += PART_COVERAGE;
    num / den
}

struct Side {
    titles: Vec<String>,
    authors: Vec<Vec<String>>,
    series: Option<String>,
    series_position: Option<f64>,
    found_by_author: Option<String>,
    words: BTreeSet<String>,
}

impl Side {
    fn of(c: &Candidate<'_>) -> Side {
        let mut titles: Vec<String> = Vec::new();
        let mut words = BTreeSet::new();
        let mut add_title = |raw: &str, titles: &mut Vec<String>| {
            let full = fold(raw);
            words.extend(full.split_whitespace().map(str::to_string));
            // The title and the title without its subtitle: *Middlemarch: A Novel* answers *middlemarch*.
            let short = raw
                .split([':', '(', '['])
                .next()
                .map(fold)
                .unwrap_or_default();
            for t in [full, short] {
                let key = title_key(&t).to_string();
                if !key.is_empty() && !titles.contains(&key) {
                    titles.push(key);
                }
            }
        };
        if let Some(t) = c.book.title.as_deref() {
            add_title(t, &mut titles);
        }
        for t in &c.facts.also_titled {
            add_title(t, &mut titles);
        }
        let authors: Vec<Vec<String>> = c
            .book
            .authors
            .iter()
            .map(|a| fold(a).split_whitespace().map(str::to_string).collect())
            .filter(|v: &Vec<String>| !v.is_empty())
            .collect();
        for a in &authors {
            words.extend(a.iter().cloned());
        }
        // A work returned for an author request carries that author's name as surely as a byline does.
        if let Some(a) = &c.facts.found_by_author {
            words.extend(a.split_whitespace().map(str::to_string));
        }
        let series = c
            .facts
            .series
            .as_deref()
            .map(fold)
            .filter(|s| !s.is_empty());
        if let Some(s) = &series {
            words.extend(s.split_whitespace().map(str::to_string));
        }
        Side {
            titles,
            authors,
            series,
            series_position: c.facts.series_position,
            found_by_author: c.facts.found_by_author.clone(),
            words,
        }
    }

    fn has_isbn(&self, book: &Book, want: &str) -> bool {
        let thirteen = |i: &str| {
            if i.len() == 13 {
                Some(i.to_string())
            } else {
                isbn10_to_13(i)
            }
        };
        let want13 = thirteen(want);
        [book.isbn_13.as_deref(), book.isbn_10.as_deref()]
            .into_iter()
            .flatten()
            .any(|have| have == want || (want13.is_some() && thirteen(have) == want13))
    }

    fn title(&self, reading_title: &str) -> f64 {
        let want = title_key(reading_title);
        self.titles
            .iter()
            .map(|t| title_similarity(want, t))
            .fold(0.0, f64::max)
    }

    fn author(&self, reading_author: &str) -> f64 {
        if self.found_by_author.as_deref() == Some(reading_author) {
            // The provider was asked for this author and answered with this work, in whatever script.
            return 1.0;
        }
        let want: Vec<&str> = reading_author.split_whitespace().collect();
        self.authors
            .iter()
            .map(|a| name_match(&want, a))
            .fold(0.0, f64::max)
    }

    fn series(&self, reading_series: &str) -> f64 {
        match &self.series {
            Some(have) => title_similarity(title_key(reading_series), title_key(have)),
            None => 0.0,
        }
    }

    fn coverage(&self, query_words: &[String]) -> f64 {
        let content: Vec<&String> = query_words.iter().filter(|w| !is_filler(w)).collect();
        let counted: Vec<&String> = if content.is_empty() {
            query_words.iter().collect()
        } else {
            content
        };
        if counted.is_empty() {
            return 0.0;
        }
        let got: f64 = counted
            .iter()
            .map(|w| {
                if self.words.contains(w.as_str()) {
                    1.0
                } else if w.chars().count() >= 4
                    && self
                        .words
                        .iter()
                        .any(|h| h.chars().count() >= 4 && jaro_winkler(w, h) >= NEAR_WORD)
                {
                    NEAR_CREDIT
                } else {
                    0.0
                }
            })
            .sum();
        got / counted.len() as f64
    }
}

fn name_match(want: &[&str], have: &[String]) -> f64 {
    if want.is_empty() || have.is_empty() {
        return 0.0;
    }
    let joined_want: String = want.concat();
    let joined_have: String = have.concat();
    if joined_want == joined_have {
        return 1.0;
    }
    let got: f64 = want
        .iter()
        .map(|w| {
            have.iter()
                .map(|h| {
                    if w == h || (w.chars().count() == 1 && h.starts_with(w)) {
                        1.0
                    } else if h.chars().count() == 1 && w.starts_with(h.as_str()) {
                        0.8
                    } else if w.chars().count() >= 4
                        && h.chars().count() >= 4
                        && jaro_winkler(w, h) >= NEAR_WORD
                    {
                        NEAR_CREDIT
                    } else if joined_have.contains(w) && w.chars().count() >= 4 {
                        0.8
                    } else {
                        0.0
                    }
                })
                .fold(0.0, f64::max)
        })
        .sum();
    got / want.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::query::{KnowsNothing, parse};

    fn book(title: &str, author: &str) -> Book {
        Book {
            title: Some(title.into()),
            authors: vec![author.into()],
            ..Default::default()
        }
    }

    fn score(q: &str, b: &Book, facts: &WorkFacts, pos: usize) -> f64 {
        let q = parse(q, &KnowsNothing);
        rank(
            &q,
            &Candidate {
                book: b,
                facts,
                sources: &[ProviderId::OpenLibrary],
                best_position: pos,
                notability: None,
            },
        )
    }

    fn editions(n: u32) -> WorkFacts {
        WorkFacts {
            edition_count: Some(n),
            ..Default::default()
        }
    }

    #[test]
    fn the_work_beats_the_study_guide_that_shares_its_title() {
        let novel = book("Il nome della rosa", "Umberto Eco");
        let novel_facts = WorkFacts {
            edition_count: Some(202),
            also_titled: vec!["The Name of the Rose".into()],
            ..Default::default()
        };
        let other = book("The name of the rose", "Lope de Vega");
        let q = "the name of the rose";
        assert!(score(q, &novel, &novel_facts, 3) > score(q, &other, &editions(2), 0));
    }

    #[test]
    fn dune_is_above_dune_messiah_for_dune() {
        let f = editions(150);
        assert!(
            score("dune", &book("Dune", "Frank Herbert"), &f, 1)
                > score("dune", &book("Dune Messiah", "Frank Herbert"), &f, 0)
        );
    }

    #[test]
    fn title_and_author_beat_a_title_alone() {
        let f = editions(10);
        let q = "beloved by toni morrison";
        assert!(
            score(q, &book("Beloved", "Toni Morrison"), &f, 1)
                > score(q, &book("Beloved, by Toni Morrison", "Mary Bailey"), &f, 0)
        );
        let q = "the plague camus";
        let la_peste = WorkFacts {
            edition_count: Some(288),
            also_titled: vec!["The Plague".into()],
            ..Default::default()
        };
        assert!(
            score(q, &book("La Peste", "Albert Camus"), &la_peste, 0)
                > score(q, &book("The Plague", "Steven G. Kellman"), &editions(4), 0)
        );
    }

    #[test]
    fn a_series_place_finds_the_book_in_it() {
        let messiah = WorkFacts {
            edition_count: Some(100),
            series: Some("Dune".into()),
            series_position: Some(2.0),
            ..Default::default()
        };
        let dune = WorkFacts {
            edition_count: Some(240),
            series: Some("Dune".into()),
            series_position: Some(1.0),
            ..Default::default()
        };
        assert!(
            score(
                "dune 2",
                &book("Dune Messiah", "Frank Herbert"),
                &messiah,
                1
            ) > score("dune 2", &book("Dune", "Frank Herbert"), &dune, 0)
        );
    }

    #[test]
    fn names_match_order_free_and_by_initial() {
        let le_guin: Vec<String> = ["ursula", "k", "le", "guin"].map(String::from).to_vec();
        assert_eq!(name_match(&["ursula", "le", "guin"], &le_guin), 1.0);
        assert_eq!(name_match(&["le", "guin", "ursula"], &le_guin), 1.0);
        assert_eq!(name_match(&["leguin"], &["le".into(), "guin".into()]), 1.0);
        assert_eq!(
            name_match(&["herbert"], &["frank".into(), "herbert".into()]),
            1.0
        );
        assert_eq!(
            name_match(&["frank", "herbert"], &["brian".into(), "herbert".into()]),
            0.5
        );
        assert_eq!(
            name_match(&["j", "tolkien"], &["john".into(), "tolkien".into()]),
            1.0
        );
    }

    #[test]
    fn an_isbn_typed_is_identity_whichever_form() {
        let mut b = book("Dune", "Frank Herbert");
        b.isbn_13 = Some("9780441172719".into());
        let none = WorkFacts::default();
        assert!(score("0441172717", &b, &none, 9) >= IDENTITY);
        assert!(score("978-0-441-17271-9", &b, &none, 9) >= IDENTITY);
        assert!(score("9780441013593", &b, &none, 0) < IDENTITY);
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::search::query::{KnowsNothing, parse};
    use proptest::prelude::*;

    fn text() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-zA-Z ]{0,24}",
            "[\\p{Latin}\\p{Han}'’,.:!-]{0,20}",
            Just(String::new()),
        ]
    }

    fn arb_book() -> impl Strategy<Value = Book> {
        (
            proptest::option::of(text()),
            proptest::collection::vec(text(), 0..3),
            proptest::option::of("[a-z]{2}"),
        )
            .prop_map(|(title, authors, language)| Book {
                title,
                authors,
                language,
                ..Default::default()
            })
    }

    fn arb_facts() -> impl Strategy<Value = WorkFacts> {
        (
            proptest::option::of(0u32..100_000),
            proptest::collection::vec(text(), 0..2),
            proptest::option::of(text()),
            proptest::option::of(0.0f64..30.0),
        )
            .prop_map(
                |(edition_count, also_titled, series, series_position)| WorkFacts {
                    edition_count,
                    also_titled,
                    series,
                    series_position,
                    languages: vec!["en".into()],
                    found_by_author: None,
                },
            )
    }

    fn sources(two: bool) -> &'static [ProviderId] {
        if two {
            &[ProviderId::OpenLibrary, ProviderId::GoogleBooks]
        } else {
            &[ProviderId::OpenLibrary]
        }
    }

    proptest! {
        #[test]
        fn rank_is_always_finite(
            q in text(),
            b in arb_book(),
            f in arb_facts(),
            pos in 0usize..1000,
            two in any::<bool>(),
        ) {
            let q = parse(&q, &KnowsNothing);
            let s = rank(&q, &Candidate { book: &b, facts: &f, sources: sources(two), best_position: pos, notability: None });
            prop_assert!(s.is_finite() && s >= 0.0, "{}", s);
            prop_assert!(s <= IDENTITY + MAX_WITHOUT_IDENTITY + 1e-9, "{}", s);
        }

        #[test]
        fn an_exact_isbn_dominates(
            rival in arb_book(),
            rival_facts in arb_facts(),
            pos_a in 0usize..50,
            pos_b in 0usize..50,
        ) {
            let isbn = "9780316228534";
            let q = parse(isbn, &KnowsNothing);
            let hit = Book { isbn_13: Some(isbn.into()), ..Default::default() };
            let mut rival = rival;
            rival.isbn_13 = None;
            rival.isbn_10 = None;
            let none = WorkFacts::default();
            let a = rank(&q, &Candidate { book: &hit, facts: &none, sources: sources(false), best_position: pos_a, notability: None });
            let b = rank(&q, &Candidate { book: &rival, facts: &rival_facts, sources: sources(true), best_position: pos_b, notability: None });
            prop_assert!(b <= MAX_WITHOUT_IDENTITY + 1e-9);
            prop_assert!(a > b, "{} vs {}", a, b);
        }

        #[test]
        fn ranking_is_invariant_to_result_order(
            q in text(),
            books in proptest::collection::vec((arb_book(), arb_facts()), 1..6),
            seed in any::<u64>(),
        ) {
            let q = parse(&q, &KnowsNothing);
            let score_all = |list: &[(Book, WorkFacts)]| -> Vec<(String, u64)> {
                let mut v: Vec<(String, u64)> = list
                    .iter()
                    .map(|(b, f)| {
                        let s = rank(&q, &Candidate { book: b, facts: f, sources: sources(false), best_position: 0, notability: None });
                        (format!("{:?}{:?}", b.title, b.authors), s.to_bits())
                    })
                    .collect();
                v.sort();
                v
            };
            let mut shuffled = books.clone();
            let n = shuffled.len();
            for i in 0..n {
                let j = (seed as usize).wrapping_mul(i + 7) % n;
                shuffled.swap(i, j);
            }
            prop_assert_eq!(score_all(&books), score_all(&shuffled));
        }

        #[test]
        fn earlier_positions_never_score_lower(
            q in text(), b in arb_book(), f in arb_facts(), x in 0usize..500, y in 0usize..500,
        ) {
            prop_assume!(x < y);
            let q = parse(&q, &KnowsNothing);
            let at = |p| rank(&q, &Candidate { book: &b, facts: &f, sources: sources(false), best_position: p, notability: None });
            prop_assert!(at(x) >= at(y));
        }

        #[test]
        fn agreement_and_standing_never_hurt(
            q in text(), b in arb_book(), f in arb_facts(), more in 0u32..1000,
        ) {
            let q = parse(&q, &KnowsNothing);
            let one = rank(&q, &Candidate { book: &b, facts: &f, sources: sources(false), best_position: 3, notability: None });
            let two = rank(&q, &Candidate { book: &b, facts: &f, sources: sources(true), best_position: 3, notability: None });
            prop_assert!(two >= one);
            let mut bigger = f.clone();
            bigger.edition_count = Some(f.edition_count.unwrap_or(0).saturating_add(more));
            let grown = rank(&q, &Candidate { book: &b, facts: &bigger, sources: sources(false), best_position: 3, notability: None });
            prop_assert!(grown >= one);
            let known = rank(&q, &Candidate { book: &b, facts: &f, sources: sources(false), best_position: 3, notability: Some(more) });
            prop_assert!(known >= one);
        }
    }
}
