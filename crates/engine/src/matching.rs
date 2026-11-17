//! Decides whether a book arriving from elsewhere is one already held.
//! The single matcher for every import path; nothing else in the engine may use `strsim` for this.
//! A match needs a shared title word; disagreeing authors or volumes veto it outright.

use std::collections::BTreeSet;

use strsim::jaro_winkler;

use crate::book::Book;
use crate::search::normalize;
use crate::volumes::Volumes;

/// At or above this a match is linked without asking. A const, not config: it defines "same book".
pub(crate) const AUTO_MATCH: f64 = 0.85;

/// Below this a match is noise; the band up to `AUTO_MATCH` is offered as a candidate.
pub(crate) const CANDIDATE_MIN: f64 = 0.60;

/// Share of the title score from character shape; the rest is shared words.
const TITLE_WEIGHT: f64 = 0.65;

// A title sharing no word cannot auto-match (`Prepared::words` relies on this).
const _: () = assert!(TITLE_WEIGHT < AUTO_MATCH);

/// Titles sharing no word differ unless near-identical (a typo of a one-word title).
/// `Middlemarch`/`Middlesex` scores 0.84 and must not pass.
const TYPO_ONLY: f64 = 0.90;

/// Authors sharing no name agree only if the strings nearly match. Kept high: character
/// scores cannot separate `J.R.R. Tolkien`/`John Ronald Reuel Tolkien` from unrelated names.
const AUTHOR_AGREE: f64 = 0.85;

/// Minimum length, in characters, of a shared name token, so a bare initial is not agreement.
const NAME_TOKEN_MIN: usize = 3;

/// Kept deliberately short: a general stopword list would strip words that are titles.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "of", "and", "or", "in", "on", "to", "for", "from", "with", "at", "by", "is",
];

pub(crate) struct Query<'a> {
    pub title: Option<&'a str>,
    pub authors: &'a [String],
}

impl<'a> Query<'a> {
    pub(crate) fn new(title: Option<&'a str>, authors: &'a [String]) -> Self {
        Self { title, authors }
    }
}

pub(crate) struct Verdict {
    pub score: f64,
    /// Whether this may be linked without asking; the author veto can withhold it.
    pub can_auto: bool,
}

pub(crate) struct Prepared {
    norm: String,
    tokens: BTreeSet<String>,
    authors: Vec<String>,
    volumes: Volumes,
}

impl Prepared {
    /// `None` for an empty title, which matches nothing.
    pub(crate) fn new(q: &Query<'_>) -> Option<Self> {
        let norm = normalize(q.title?);
        if norm.is_empty() {
            return None;
        }
        Some(Self {
            tokens: content_tokens(&norm),
            volumes: Volumes::of(&norm),
            norm,
            authors: q.authors.iter().map(|a| author_key(a)).collect(),
        })
    }

    /// `None` when the book is not a candidate at all: no shared word, or a different author.
    pub(crate) fn compare(&self, book: &Book) -> Option<Verdict> {
        let have = normalize(book.title.as_deref().unwrap_or(""));
        if have.is_empty() {
            return None;
        }
        let theirs = content_tokens(&have);
        let authors: Vec<String> = book.authors.iter().map(|a| author_key(a)).collect();
        self.verdict(&have, &theirs, &authors, &Volumes::of(&have))
    }

    pub(crate) fn compare_prepared(&self, other: &Prepared) -> Option<Verdict> {
        self.verdict(&other.norm, &other.tokens, &other.authors, &other.volumes)
    }

    /// The content words. An auto match shares one: with none, the score is at most
    /// [`TITLE_WEIGHT`], below [`AUTO_MATCH`].
    pub(crate) fn words(&self) -> impl Iterator<Item = &str> {
        self.tokens.iter().map(String::as_str)
    }

    /// Volumes are read off the normalized title on both sides so both compare paths agree.
    fn verdict(
        &self,
        have: &str,
        theirs: &BTreeSet<String>,
        authors: &[String],
        volumes: &Volumes,
    ) -> Option<Verdict> {
        let chars = jaro_winkler(&self.norm, have);
        let shared = self.tokens.intersection(theirs).count();
        if shared == 0 && chars < TYPO_ONLY {
            return None;
        }
        let dice = if self.tokens.is_empty() || theirs.is_empty() {
            0.0
        } else {
            2.0 * shared as f64 / (self.tokens.len() + theirs.len()) as f64
        };

        if authors_disagree(&self.authors, authors) {
            return None;
        }
        if self.volumes.differ(volumes) {
            return None;
        }

        let score = TITLE_WEIGHT * chars + (1.0 - TITLE_WEIGHT) * dice;
        Some(Verdict {
            score,
            can_auto: score >= AUTO_MATCH,
        })
    }
}

/// The title blend over two already-normalized titles, with no veto and no `None`.
pub(crate) fn title_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let chars = jaro_winkler(a, b);
    let (ta, tb) = (content_tokens(a), content_tokens(b));
    let shared = ta.intersection(&tb).count();
    let dice = if ta.is_empty() || tb.is_empty() {
        0.0
    } else {
        2.0 * shared as f64 / (ta.len() + tb.len()) as f64
    };
    TITLE_WEIGHT * chars + (1.0 - TITLE_WEIGHT) * dice
}

/// Falls back to every word when the title is all stopwords (`The End` must match itself).
fn content_tokens(normalized: &str) -> BTreeSet<String> {
    let all: BTreeSet<String> = normalized.split_whitespace().map(str::to_string).collect();
    let content: BTreeSet<String> = all
        .iter()
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .cloned()
        .collect();
    if content.is_empty() { all } else { content }
}

/// Author name as sorted tokens, so `George Eliot` and `Eliot, George` agree.
/// Not `search::normalize`: that drops a leading article and would eat an initial `A.`.
fn author_key(name: &str) -> String {
    let mut tokens: Vec<String> = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();
    tokens.sort();
    tokens.join(" ")
}

/// Whether two author lists are certainly different people. A veto only: empty on either
/// side is not disagreement, and "same person" is not decidable here.
fn authors_disagree(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    !a.iter().any(|x| b.iter().any(|y| one_author_agrees(x, y)))
}

fn one_author_agrees(x: &str, y: &str) -> bool {
    let xs: BTreeSet<&str> = x.split_whitespace().collect();
    let shared = y
        .split_whitespace()
        .any(|w| w.chars().count() >= NAME_TOKEN_MIN && xs.contains(w));
    shared || jaro_winkler(x, y) >= AUTHOR_AGREE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(title: &str, authors: &[&str]) -> Book {
        Book {
            title: Some(title.into()),
            authors: authors.iter().map(|a| (*a).to_string()).collect(),
            ..Default::default()
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        Drop,
        Band,
        Auto,
    }

    fn outcome(q: (&str, &[&str]), b: (&str, &[&str])) -> Outcome {
        let authors: Vec<String> = q.1.iter().map(|a| (*a).to_string()).collect();
        let query = Query::new(Some(q.0), &authors);
        match Prepared::new(&query).and_then(|p| p.compare(&book(b.0, b.1))) {
            None => Outcome::Drop,
            Some(v) if v.can_auto => Outcome::Auto,
            Some(v) if v.score >= CANDIDATE_MIN => Outcome::Band,
            Some(_) => Outcome::Drop,
        }
    }

    #[test]
    fn unrelated_works_are_not_offered_and_variants_still_are() {
        let none: &[&str] = &[];
        let cases: &[(&str, &str, Outcome)] = &[
            ("Middlemarch", "Middlesex", Outcome::Drop),
            ("Kafka on the Shore", "Klara and the Sun", Outcome::Drop),
            (
                "The Left Hand of Darkness",
                "The Lathe of Heaven",
                Outcome::Drop,
            ),
            ("The Sea, The Sea", "The Selfish Gene", Outcome::Drop),
            ("Blood Meridian", "Blindness", Outcome::Drop),
            (
                "Middlemarch",
                "Middlemarch: A Study of Provincial Life",
                Outcome::Band,
            ),
            (
                "Station Eleven",
                "Station Eleven: A Novel of Survival and the Travelling Symphony",
                Outcome::Band,
            ),
            (
                "Sapiens: A Brief History of Humankind",
                "Sapiens",
                Outcome::Band,
            ),
            // Same author and series, different book: must not auto-link.
            ("Dune", "Dune Messiah", Outcome::Band),
            ("Slaughterhouse-Five", "Slaughterhouse Five", Outcome::Auto),
            ("Neuromancer", "neuromancer", Outcome::Auto),
            ("The Dispossessed", "Dispossessed", Outcome::Auto),
        ];
        for (a, b, want) in cases {
            assert_eq!(outcome((a, none), (b, none)), *want, "{a:?} against {b:?}");
        }
    }

    #[test]
    fn two_volumes_of_a_set_are_two_books_and_two_editions_are_one() {
        let poe: &[&str] = &["Edgar Allan Poe"];
        let xen: &[&str] = &["Xenophon"];
        let goethe: &[&str] = &["Johann Wolfgang von Goethe"];
        let austen: &[&str] = &["Jane Austen"];
        assert_eq!(
            outcome(
                ("The Works of Edgar Allan Poe — Volume 2", poe),
                ("The Works of Edgar Allan Poe — Volume 1", poe)
            ),
            Outcome::Drop
        );
        assert_eq!(
            outcome(
                ("Κύρου Ανάβασις Τόμος 2", xen),
                ("Κύρου Ανάβασις Τόμος 1", xen)
            ),
            Outcome::Drop
        );
        assert_eq!(
            outcome(
                ("Faust: Eine Tragödie [erster Teil]", goethe),
                ("Faust: Der Tragödie erster Teil", goethe)
            ),
            Outcome::Auto
        );
        assert_eq!(
            outcome(
                ("Pride and Prejudice", austen),
                ("Pride and Prejudice", austen)
            ),
            Outcome::Auto
        );
        let none: &[&str] = &[];
        assert_eq!(
            outcome(
                ("Harry Potter and the Philosopher's Stone", none),
                (
                    "Harry Potter and the Philosopher's Stone (Harry Potter, Book 1)",
                    none
                )
            ),
            Outcome::Auto
        );
        assert_eq!(
            outcome(
                ("Les misérables, Tome I: Fantine", none),
                ("Les misérables Tome 1: Fantine", none)
            ),
            Outcome::Auto
        );
    }

    #[test]
    fn a_near_identical_string_survives_sharing_no_whole_word() {
        let none: &[&str] = &[];
        assert_ne!(
            outcome(("Neuromancer", none), ("Neuromacer", none)),
            Outcome::Drop
        );
    }

    #[test]
    fn an_author_who_is_somebody_else_ends_it() {
        assert_eq!(
            outcome(
                ("The Art of War", &["Sun Tzu"]),
                ("The Art of Racing in the Rain", &["Garth Stein"])
            ),
            Outcome::Drop
        );
        assert_eq!(
            outcome(
                ("Middlemarch", &["George Eliot"]),
                ("Middlemarch", &["Someone Else Entirely"])
            ),
            Outcome::Drop,
            "0.69 as strings — jaro-winkler's floor is no lower on names than on titles, \
             which is why a shared *name* and not a character score is what agrees"
        );
        assert_ne!(
            outcome(
                ("Good Omens", &["Terry Pratchett", "Neil Gaiman"]),
                ("Good Omens", &["Neil Gaiman"])
            ),
            Outcome::Drop
        );
        let none: &[&str] = &[];
        assert_eq!(
            outcome(
                ("The Art of War", none),
                ("The Art of Racing in the Rain", none)
            ),
            Outcome::Band
        );
    }

    #[test]
    fn the_same_author_written_both_ways_is_one_person() {
        assert_eq!(
            outcome(
                ("Middlemarch", &["George Eliot"]),
                ("Middlemarch", &["Eliot, George"])
            ),
            Outcome::Auto
        );
        assert_eq!(
            outcome(
                ("The Hobbit", &["J.R.R. Tolkien"]),
                ("The Hobbit", &["John Ronald Reuel Tolkien"])
            ),
            Outcome::Auto
        );
        assert_eq!(
            outcome(
                ("The Dispossessed", &["Ursula K. Le Guin"]),
                ("The Dispossessed", &["Ursula Le Guin"])
            ),
            Outcome::Auto
        );
    }

    #[test]
    fn a_title_of_nothing_but_stopwords_matches_itself() {
        let none: &[&str] = &[];
        assert_eq!(outcome(("The End", none), ("The End", none)), Outcome::Auto);
        assert_eq!(outcome(("It", none), ("It", none)), Outcome::Auto);
    }

    #[test]
    fn an_empty_query_prepares_to_nothing() {
        assert!(Prepared::new(&Query::new(None, &[])).is_none());
        assert!(Prepared::new(&Query::new(Some("   "), &[])).is_none());
        assert!(Prepared::new(&Query::new(Some("!!!"), &[])).is_none());
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn title() -> impl Strategy<Value = String> {
            "[a-zA-Z ]{1,40}"
        }

        proptest! {
            #[test]
            fn a_title_matches_itself_exactly(t in title()) {
                let authors: Vec<String> = Vec::new();
                let q = Query::new(Some(&t), &authors);
                if let Some(p) = Prepared::new(&q) {
                    let v = p.compare(&book(&t, &[])).expect("a book is not a coincidence");
                    prop_assert!((v.score - 1.0).abs() < 1e-9, "{}", v.score);
                    prop_assert!(v.can_auto);
                }
            }

            #[test]
            fn the_score_is_always_a_fraction(a in title(), b in title()) {
                let authors: Vec<String> = Vec::new();
                let q = Query::new(Some(&a), &authors);
                if let Some(p) = Prepared::new(&q)
                    && let Some(v) = p.compare(&book(&b, &[])) {
                    prop_assert!(v.score.is_finite());
                    prop_assert!((0.0..=1.0).contains(&v.score), "{}", v.score);
                    prop_assert_eq!(v.can_auto, v.score >= AUTO_MATCH);
                }
            }

            #[test]
            fn prepared_and_raw_comparisons_agree(
                a in title(),
                b in title(),
                qa in proptest::collection::vec("[A-Za-z. ,]{0,16}", 0..3),
                ba in proptest::collection::vec("[A-Za-z. ,]{0,16}", 0..3),
            ) {
                let book = Book {
                    title: Some(b.clone()),
                    authors: ba.clone(),
                    ..Default::default()
                };
                let q = Prepared::new(&Query::new(Some(&a), &qa));
                let other = Prepared::new(&Query::new(Some(&b), &ba));
                let raw = q.as_ref().and_then(|p| p.compare(&book)).map(|v| (v.score, v.can_auto));
                let prepared = match (&q, &other) {
                    (Some(p), Some(o)) => p.compare_prepared(o).map(|v| (v.score, v.can_auto)),
                    _ => None,
                };
                prop_assert_eq!(raw, prepared);
            }

            #[test]
            fn different_volumes_never_match(
                t in "[a-zA-Z]{3,12}( [a-zA-Z]{3,12}){0,4}",
                n in 1u32..=40,
                m in 1u32..=40,
            ) {
                prop_assume!(crate::volumes::Volumes::of(&t) == Default::default());
                let a = format!("{t} Volume {n}");
                let b = format!("{t} Volume {m}");
                let authors: Vec<String> = Vec::new();
                let v = Prepared::new(&Query::new(Some(&a), &authors))
                    .and_then(|p| p.compare(&book(&b, &[])));
                if n == m {
                    prop_assert!(v.is_some_and(|v| v.can_auto), "{a} / {b}");
                } else {
                    prop_assert!(v.is_none(), "{a} / {b}");
                }
            }

            #[test]
            fn the_comparison_is_symmetric(a in title(), b in title()) {
                let authors: Vec<String> = Vec::new();
                let forward = Prepared::new(&Query::new(Some(&a), &authors))
                    .and_then(|p| p.compare(&book(&b, &[])).map(|v| v.score));
                let back = Prepared::new(&Query::new(Some(&b), &authors))
                    .and_then(|p| p.compare(&book(&a, &[])).map(|v| v.score));
                match (forward, back) {
                    (Some(x), Some(y)) => prop_assert!((x - y).abs() < 1e-9, "{x} vs {y}"),
                    (None, None) => {}
                    (x, y) => prop_assert!(false, "one side dropped it: {x:?} vs {y:?}"),
                }
            }
        }
    }
}
