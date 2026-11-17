//! Is this the book I already have?
//!
//! One rule, shared by every path that meets a book from somewhere else —
//! calibre, Goodreads, a KOReader sidecar, an owned file. There is deliberately
//! no second matcher — two would give two answers to "is this the same book"
//! depending on where the book came in — and this module is where the one
//! lives; nothing else in the engine may reach for `strsim` to answer this
//! question.
//!
//! **Why this is not a bare jaro-winkler on the titles**, which is what it was.
//! Jaro-Winkler is a character-transposition measure with a four-character
//! prefix bonus, built for personal names. On a whole title three things
//! compound:
//!
//! 1. [`crate::search::normalize`] drops a leading article, which makes a
//!    *shared first word* more likely, which is exactly what the prefix bonus
//!    rewards.
//! 2. `0.60` is not a weak threshold for that metric — for two English titles it
//!    is roughly "both are English". Measured over 780 pairs of real titles,
//!    **10% landed in the 0.60..0.85 band**.
//! 3. The score reported is the *maximum over the whole library*, so it is an
//!    extreme-value draw. At fifty books the chance that something scores into
//!    the band is 99.5%, and the number is then printed as a percentage that
//!    looks like a measurement.
//!
//! So every unmatched row grew a confident wrong candidate — `Middlemarch` against
//! `Middlesex` at 84%, `The Left Hand of Darkness` against `The Lathe of
//! Heaven` at 71% — and `Dune` against `Dune Messiah` at 87% linked itself,
//! silently, to the wrong book. Two signals fix it: the titles must share a
//! *word*, and the authors must not disagree.
//!
//! **A third veto: the volumes must not disagree** (2026-11-13). *The Works of
//! Edgar Allan Poe — Volume 2* shares every word but its number with *Volume
//! 1* and scored over the auto line, so a folder holding a set filed the
//! second volume as a copy of the first. Two titles that each name a volume,
//! part or book and name different ones are different books — not a weaker
//! candidate, no candidate. The reading of a title's numbers is
//! [`crate::volumes`]; this module only asks it.

use std::collections::BTreeSet;

use strsim::jaro_winkler;

use crate::book::Book;
use crate::search::normalize;
use crate::volumes::Volumes;

/// At or above this a match is made with no questions asked.
///
/// `const`, not config, for the same reason `PROVIDER_TIMEOUT` is: a knob here
/// would be a knob on what "the same book" means, and the only honest way to
/// test either value is against a fixed one.
pub(crate) const AUTO_MATCH: f64 = 0.85;

/// Below this a match is noise, not a candidate. The band between the two is
/// what a caller offers as *maybe this one*: too weak to link silently, too
/// strong to throw away, which is exactly the case where a variant title used
/// to become a duplicate with nothing said.
pub(crate) const CANDIDATE_MIN: f64 = 0.60;

/// How the blended title score splits between the two halves — character shape
/// and shared words.
///
/// The character half still carries most of it, because it is what recognises a
/// hyphen, a spelling and a stray subtitle colon. The word half is what stops a
/// coincidence of letters reading as a book.
const TITLE_WEIGHT: f64 = 0.65;

// `Prepared::words`' promise: a title sharing no word cannot auto-match.
const _: () = assert!(TITLE_WEIGHT < AUTO_MATCH);

/// Two titles sharing no word at all are different books — **unless** the
/// strings are near-identical, which is a misspelling of a one-word title
/// rather than a coincidence. `Middlemarch`/`Middlesex` is 0.84 and must not
/// survive; a genuine typo is above this.
const TYPO_ONLY: f64 = 0.90;

/// Two authors sharing no name agree only if the whole strings nearly match.
///
/// High, and it has to be: jaro-winkler's floor is the reason this module
/// exists, and it is no lower on names than on titles — `George Eliot` against
/// `Someone Else Entirely` is 0.69, which is *above* `J.R.R. Tolkien` against
/// `John Ronald Reuel Tolkien` at 0.65. No character threshold separates those
/// two, so the character measure is only trusted where it is nearly certain
/// (`Dostoevsky`/`Dostoyevsky`), and a **shared name** is what carries the
/// ordinary case.
const AUTHOR_AGREE: f64 = 0.85;

/// How long a shared name token has to be to count as the same person.
///
/// Three characters, so a bare initial is not agreement — `J. Smith` and
/// `J. Nguyen` share `j` and nothing else. In characters rather than bytes,
/// because a two-character CJK name is a whole name; that case is caught by the
/// [`AUTHOR_AGREE`] fallback instead, which is exact on identical strings.
const NAME_TOKEN_MIN: usize = 3;

/// Words that carry no identity, so overlap on them is not evidence.
///
/// A deliberately short list: it exists so that `The Art of War` and `The Art
/// of Racing in the Rain` are judged on `art`/`war` against `art`/`racing`/
/// `rain` rather than on the `of` they share. Lengthening it into a general
/// English stopword list would start removing words that *are* titles.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "of", "and", "or", "in", "on", "to", "for", "from", "with", "at", "by", "is",
];

/// One side of a comparison: what the other system says it has.
pub(crate) struct Query<'a> {
    pub title: Option<&'a str>,
    pub authors: &'a [String],
}

impl<'a> Query<'a> {
    pub(crate) fn new(title: Option<&'a str>, authors: &'a [String]) -> Self {
        Self { title, authors }
    }
}

/// What one comparison decided.
pub(crate) struct Verdict {
    /// The blended title score, `0.0..=1.0`. This is the number a frontend
    /// shows as a percentage, so it has to mean something on its own.
    pub score: f64,
    /// Whether this may be linked without asking. Separate from `score` because
    /// the author veto can withhold it, and because a caller that only looked at
    /// the number would have to re-derive the rule.
    pub can_auto: bool,
}

/// The query side, worked out once.
///
/// A scan compares one query against every book in the library, so normalizing
/// and tokenizing the query per book would be the same work ten thousand times.
pub(crate) struct Prepared {
    norm: String,
    tokens: BTreeSet<String>,
    authors: Vec<String>,
    volumes: Volumes,
}

impl Prepared {
    /// `None` when there is no title to match on — an empty query matches
    /// nothing rather than everything.
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

    /// `None` when this book is not a candidate at all — no shared word, or an
    /// author who is somebody else. That is the half of the fix that matters:
    /// the old matcher had no way to say *nothing here looks like it*, so it
    /// always named its best coincidence.
    pub(crate) fn compare(&self, book: &Book) -> Option<Verdict> {
        let have = normalize(book.title.as_deref().unwrap_or(""));
        if have.is_empty() {
            return None;
        }
        let theirs = content_tokens(&have);
        let authors: Vec<String> = book.authors.iter().map(|a| author_key(a)).collect();
        self.verdict(&have, &theirs, &authors, &Volumes::of(&have))
    }

    /// [`Prepared::compare`] against a side that is prepared too — the same
    /// rule, with neither side normalized again.
    ///
    /// For *you have this* (`search::held`), which meets every work of a search
    /// against every book the app holds: the library is prepared **once per
    /// search** (never once per row: the mistake `calibre.rs` records), and normalizing
    /// each held title once per work would undo that. `compare` and this reach
    /// the same [`Prepared::verdict`], so the two cannot disagree about what
    /// "the same book" means; `prepared_and_raw_comparisons_agree` pins it.
    pub(crate) fn compare_prepared(&self, other: &Prepared) -> Option<Verdict> {
        self.verdict(&other.norm, &other.tokens, &other.authors, &other.volumes)
    }

    /// The title's content words. **An auto match shares one**: with none the
    /// word half of the blend is zero and the score is at most
    /// [`TITLE_WEIGHT`], below [`AUTO_MATCH`] (asserted at compile time
    /// beside it), so a caller looking only for auto matches may compare only
    /// the books that share a word without changing a single answer.
    pub(crate) fn words(&self) -> impl Iterator<Item = &str> {
        self.tokens.iter().map(String::as_str)
    }

    /// The rule itself, over the other side's normalized title, its content
    /// tokens, its keyed authors and the volumes its title names.
    ///
    /// The volumes are read off the **normalized** title on both sides, so
    /// `compare` and `compare_prepared` read the same string and cannot
    /// disagree about them.
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

/// The title blend [`Prepared::compare`] scores with — character shape plus
/// shared content words — between two titles **the caller has already
/// normalized**, with no veto and no `None`: `0.0..=1.0`.
///
/// For the search ranking (`search::ranking`), which folds titles its own way
/// (`search::query::fold`) and wants a grade rather than a verdict: a result sharing no word
/// with the query is a poor match, not "not a candidate". Nothing in this
/// module calls it, so no import path's answer can move because of it.
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

/// The words of an already-normalized title that carry identity.
///
/// Falls back to every word when the title is nothing but stopwords — `The
/// End` is a real title, and a book with no tokens at all could never match
/// itself.
fn content_tokens(normalized: &str) -> BTreeSet<String> {
    let all: BTreeSet<String> = normalized.split_whitespace().map(str::to_string).collect();
    let content: BTreeSet<String> = all
        .iter()
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .cloned()
        .collect();
    if content.is_empty() { all } else { content }
}

/// An author name reduced to something two systems can agree on.
///
/// **Sorted tokens**, because the same person arrives as `George Eliot` from
/// calibre and `Eliot, George` from an epub's `author_sort`, and a straight
/// comparison of those two scores like different people. Sorting makes both
/// `eliot george`.
///
/// Not [`crate::search::normalize`]: that drops a leading article, and `A. N.
/// Writer` would lose its first initial.
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

/// Whether these two author lists are certainly different people.
///
/// **A veto, and only a veto.** It is asked one question — *is this definitely
/// somebody else?* — because the question it is not asked, "are these the same
/// person", has no answer here: `J.R.R. Tolkien` and `John Ronald Reuel
/// Tolkien` are one person, `Frank Herbert` and `Brian Herbert` are two, and
/// nothing short of an authority file tells them apart. Getting that wrong in
/// the permissive direction costs a candidate the user can decline; getting it
/// wrong the other way hides a book they own.
///
/// Empty on either side is **not** disagreement, and that is the common case
/// rather than an edge one: a book pulled from a sidecar, imported from a bare
/// filename, or seeded by a test has no author at all.
///
/// Two names agree if they **share a name** — which is what survives an initial
/// being expanded, dropped, or moved behind a comma — or, failing that, if the
/// whole strings nearly match.
fn authors_disagree(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    !a.iter().any(|x| b.iter().any(|y| one_author_agrees(x, y)))
}

/// One pair of already-keyed names.
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

    /// What one comparison came to, named the way the callers think about it.
    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        /// Not offered at all.
        Drop,
        /// Offered as *maybe this one* — a decision, not a link.
        Band,
        /// Linked with no question asked.
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

    /// The table this module exists for. Every `Drop` row scored 0.66-0.87
    /// under the old bare jaro-winkler and was offered — the `Dune` row linked
    /// itself.
    #[test]
    fn unrelated_works_are_not_offered_and_variants_still_are() {
        let none: &[&str] = &[];
        let cases: &[(&str, &str, Outcome)] = &[
            // Coincidences of letters. All of these used to be candidates.
            ("Middlemarch", "Middlesex", Outcome::Drop),
            ("Kafka on the Shore", "Klara and the Sun", Outcome::Drop),
            (
                "The Left Hand of Darkness",
                "The Lathe of Heaven",
                Outcome::Drop,
            ),
            ("The Sea, The Sea", "The Selfish Gene", Outcome::Drop),
            ("Blood Meridian", "Blindness", Outcome::Drop),
            // The shape the band is *for*: a subtitle one side carries and the
            // other does not.
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
            // Same author, same series, different book. This is the one that
            // used to auto-link, which is the worst outcome available: a
            // reading history quietly filed under the wrong book.
            ("Dune", "Dune Messiah", Outcome::Band),
            // Punctuation and case are not a difference.
            ("Slaughterhouse-Five", "Slaughterhouse Five", Outcome::Auto),
            ("Neuromancer", "neuromancer", Outcome::Auto),
            ("The Dispossessed", "Dispossessed", Outcome::Auto),
        ];
        for (a, b, want) in cases {
            assert_eq!(outcome((a, none), (b, none)), *want, "{a:?} against {b:?}");
        }
    }

    /// The four pairs of `corpus/epub` that share a title, as their own
    /// metadata spells them. Two are one book in two editions and must still
    /// link; two are two volumes of a set, and the second used to be filed as
    /// a copy of the first (found 2026-11-13, importing `corpus/epub` as a
    /// folder).
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
        // A volume number on one side only is not a disagreement: the title
        // without one has said nothing about which volume it is.
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
        // The same volume written two ways is still the same volume.
        assert_eq!(
            outcome(
                ("Les misérables, Tome I: Fantine", none),
                ("Les misérables Tome 1: Fantine", none)
            ),
            Outcome::Auto
        );
    }

    /// A one-word title misspelled is still that book, and the shared-word rule
    /// would otherwise be the end of it.
    #[test]
    fn a_near_identical_string_survives_sharing_no_whole_word() {
        let none: &[&str] = &[];
        assert_ne!(
            outcome(("Neuromancer", none), ("Neuromacer", none)),
            Outcome::Drop
        );
    }

    /// The author's whole job: a veto where the titles alone would have
    /// offered a book by somebody else.
    #[test]
    fn an_author_who_is_somebody_else_ends_it() {
        assert_eq!(
            outcome(
                ("The Art of War", &["Sun Tzu"]),
                ("The Art of Racing in the Rain", &["Garth Stein"])
            ),
            Outcome::Drop
        );
        // Two books can share a title outright. Folding them together on that
        // alone is worse than leaving both on the shelf.
        assert_eq!(
            outcome(
                ("Middlemarch", &["George Eliot"]),
                ("Middlemarch", &["Someone Else Entirely"])
            ),
            Outcome::Drop,
            "0.69 as strings — jaro-winkler's floor is no lower on names than on titles, \
             which is why a shared *name* and not a character score is what agrees"
        );
        // One author in common out of several is agreement: an anthology and a
        // collaboration are not two different books.
        assert_ne!(
            outcome(
                ("Good Omens", &["Terry Pratchett", "Neil Gaiman"]),
                ("Good Omens", &["Neil Gaiman"])
            ),
            Outcome::Drop
        );
        // And with the authors unknown it is the title's call again, because
        // absence is not disagreement.
        let none: &[&str] = &[];
        assert_eq!(
            outcome(
                ("The Art of War", none),
                ("The Art of Racing in the Rain", none)
            ),
            Outcome::Band
        );
    }

    /// calibre says `George Eliot`, an epub's `author_sort` says `Eliot, George`.
    /// Compared as written those two score like different people, and the veto
    /// would fire on the commonest pair of all.
    #[test]
    fn the_same_author_written_both_ways_is_one_person() {
        assert_eq!(
            outcome(
                ("Middlemarch", &["George Eliot"]),
                ("Middlemarch", &["Eliot, George"])
            ),
            Outcome::Auto
        );
        // A surname is what survives every way a name gets rewritten, and it is
        // the only thing these two have in common.
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

    /// A title with nothing but stopwords in it still has to be able to match
    /// itself — the content-word fallback is what makes that true.
    #[test]
    fn a_title_of_nothing_but_stopwords_matches_itself() {
        let none: &[&str] = &[];
        assert_eq!(outcome(("The End", none), ("The End", none)), Outcome::Auto);
        assert_eq!(outcome(("It", none), ("It", none)), Outcome::Auto);
    }

    /// An empty or absent title matches nothing rather than everything.
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
            /// A book is itself. Not a tautology here: the score is blended out
            /// of two measures, and either one failing to reach 1.0 on equal
            /// input would make an exact title stop auto-matching.
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

            /// The score is a percentage the moment a frontend prints it, so it
            /// has to be finite and inside the range whatever it was handed.
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

            /// The prepared-against-prepared form is the same rule as the
            /// prepared-against-a-book one, authors and all: *you have this*
            /// (`search::held`) uses the first and every import path the
            /// second, and they must never disagree about one pair.
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

            /// Two volumes of one set are never the same book, never even a
            /// candidate, however alike the rest of the title — while the same
            /// volume is still the book itself.
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

            /// Which side asked must not change the answer. A calibre row
            /// scored against the library and the library scored against the
            /// row are the same question.
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
