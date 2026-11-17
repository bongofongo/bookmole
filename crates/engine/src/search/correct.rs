//! Corrects a misspelt query to the work it most likely meant; pure over the query and candidates.
//! Near miss: one edit (two past seven letters); the first letter stays or swaps with the second.

use std::collections::HashSet;

use strsim::osa_distance;

use super::query::{ParsedQuery, ReadingKind, fold, is_filler, title_key};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Target {
    pub title: String,
    pub author: Option<String>,
    pub ol_works: Vec<String>,
    pub notability: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fix {
    pub searched: String,
    pub title: String,
    pub author: Option<String>,
    pub ol_works: Vec<String>,
    pub notability: Option<u32>,
    pub typos: usize,
    pub distance: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Verdict {
    Nothing,
    Exact,
    Fix(Fix),
}

/// A sentence pasted into the box is not a misspelt title.
const MAX_WORDS: usize = 12;
const MAX_TYPOS: usize = 3;

pub(crate) fn correct(q: &ParsedQuery, targets: &[Target]) -> Verdict {
    let fielded = q.readings.iter().any(|r| r.kind == ReadingKind::Fielded);
    if q.isbn.is_some() || fielded || q.words.is_empty() || q.words.len() > MAX_WORDS {
        return Verdict::Nothing;
    }
    let mut best: Option<Fix> = None;
    for t in targets {
        match fit(&q.words, t) {
            Fit::Exact => return Verdict::Exact,
            Fit::None => {}
            Fit::Near(f) => {
                let better = match &best {
                    None => true,
                    Some(b) => {
                        (
                            f.distance,
                            f.typos,
                            std::cmp::Reverse(f.notability),
                            &f.title,
                        ) < (
                            b.distance,
                            b.typos,
                            std::cmp::Reverse(b.notability),
                            &b.title,
                        )
                    }
                };
                if better {
                    best = Some(f);
                }
            }
        }
    }
    best.map_or(Verdict::Nothing, Verdict::Fix)
}

enum Fit {
    None,
    Exact,
    Near(Fix),
}

fn fit(words: &[String], t: &Target) -> Fit {
    let title_folded = fold(&t.title);
    let title: Vec<&str> = title_folded.split_whitespace().collect();
    let author_folded = t.author.as_deref().map(fold).unwrap_or_default();
    let author: Vec<&str> = author_folded.split_whitespace().collect();
    if title.is_empty() {
        return Fit::None;
    }
    let targets: Vec<(&str, bool)> = title
        .iter()
        .map(|w| (*w, true))
        .chain(author.iter().map(|w| (*w, false)))
        .collect();
    let mut used = vec![false; targets.len()];
    let mut matched: Vec<Option<(usize, usize)>> = vec![None; words.len()];
    // Exact words first, so a near miss never takes a word an exact one needs.
    for (i, w) in words.iter().enumerate() {
        if let Some(j) = (0..targets.len()).find(|&j| !used[j] && targets[j].0 == w) {
            used[j] = true;
            matched[i] = Some((j, 0));
        }
    }
    for (i, w) in words.iter().enumerate() {
        if matched[i].is_some() {
            continue;
        }
        let pick = (0..targets.len())
            .filter(|&j| !used[j])
            .filter_map(|j| near(w, targets[j].0).map(|d| (d, !targets[j].1, j)))
            .min();
        if let Some((d, _, j)) = pick {
            used[j] = true;
            matched[i] = Some((j, d));
        }
    }
    if matched.iter().any(Option::is_none) {
        return Fit::None;
    }
    let key = title_key(&title_folded);
    let skip_article = title.len() > key.split_whitespace().count();
    let content: Vec<usize> = (0..title.len())
        .filter(|&j| !(skip_article && j == 0))
        .filter(|&j| !is_filler(title[j]))
        .collect();
    let needed = if content.is_empty() {
        (0..title.len()).collect::<Vec<_>>()
    } else {
        content
    };
    if needed.iter().any(|&j| !used[j]) {
        return Fit::None;
    }
    let misses: Vec<usize> = matched
        .iter()
        .flatten()
        .map(|(_, d)| *d)
        .filter(|d| *d > 0)
        .collect();
    if misses.is_empty() {
        return Fit::Exact;
    }
    if misses.len() > MAX_TYPOS {
        return Fit::None;
    }
    let searched: Vec<&str> = words
        .iter()
        .zip(&matched)
        .map(|(w, m)| match m {
            Some((j, d)) if *d > 0 => targets[*j].0,
            _ => w.as_str(),
        })
        .collect();
    Fit::Near(Fix {
        searched: searched.join(" "),
        title: t.title.clone(),
        author: t.author.clone(),
        ol_works: t.ol_works.clone(),
        notability: t.notability,
        typos: misses.len(),
        distance: misses.iter().sum(),
    })
}

pub(crate) fn near(typed: &str, word: &str) -> Option<usize> {
    if typed == word {
        return Some(0);
    }
    let a: Vec<char> = typed.chars().collect();
    let b: Vec<char> = word.chars().collect();
    if b.len() < 3 || a.is_empty() {
        return None;
    }
    if a.iter().chain(b.iter()).any(|c| c.is_ascii_digit()) {
        return None;
    }
    if a.len() <= 2 {
        // `ry` -> `rye`: grown by one letter at the end only.
        return (b.len() == a.len() + 1 && b.starts_with(&a)).then_some(1);
    }
    let first_kept = a[0] == b[0] || (a.len() > 1 && b.len() > 1 && a[0] == b[1] && a[1] == b[0]);
    if !first_kept {
        return None;
    }
    let budget = if a.len() >= 8 { 2 } else { 1 };
    let d = osa_distance(typed, word);
    (d <= budget).then_some(d)
}

/// A result with no title matches by Open Library id, which is not trusted beside a title.
pub(crate) fn is_fixed_work(
    fix: &Fix,
    ol_key: Option<&str>,
    titles: &[&str],
    authors: &[String],
) -> bool {
    if titles.is_empty() {
        return ol_key.is_some_and(|k| {
            let bare = k.trim_start_matches("/works/");
            fix.ol_works.iter().any(|o| o == bare)
        });
    }
    let want = fold(&fix.title);
    let want = title_key(&want);
    let same_title = titles.iter().any(|t| {
        let f = fold(t.split([':', '(']).next().unwrap_or(t));
        title_key(&f) == want
    });
    if !same_title {
        return false;
    }
    match fix.author.as_deref().map(fold) {
        None => true,
        Some(a) => {
            let want: HashSet<&str> = a.split_whitespace().collect();
            authors.is_empty()
                || authors.iter().any(|x| {
                    let f = fold(x);
                    f.split_whitespace()
                        .any(|w| w.len() > 1 && want.contains(w))
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::query::{KnowsNothing, parse};

    fn t(title: &str, author: &str, n: u32) -> Target {
        Target {
            title: title.into(),
            author: Some(author.into()),
            ol_works: Vec::new(),
            notability: Some(n),
        }
    }

    fn shelf() -> Vec<Target> {
        vec![
            t("Dune", "Frank Herbert", 77),
            t("Dune Messiah", "Frank Herbert", 40),
            t("The Hobbit", "J. R. R. Tolkien", 117),
            t("The Habit", "Someone Else", 3),
            t("The Catcher in the Rye", "J. D. Salinger", 90),
            t("The Road", "Cormac McCarthy", 60),
            t("The Hitchhiker's Guide to the Galaxy", "Douglas Adams", 80),
            t("A Wizard of Earthsea", "Ursula K. Le Guin", 70),
        ]
    }

    fn fix_of(text: &str) -> Option<String> {
        match correct(&parse(text, &KnowsNothing), &shelf()) {
            Verdict::Fix(f) => Some(f.searched),
            _ => None,
        }
    }

    #[test]
    fn a_misspelling_is_fixed_word_by_word() {
        assert_eq!(fix_of("dnue herbrt").as_deref(), Some("dune herbert"));
        assert_eq!(fix_of("the hobit").as_deref(), Some("the hobbit"));
        assert_eq!(
            fix_of("catcher in the ry").as_deref(),
            Some("catcher in the rye")
        );
        assert_eq!(
            fix_of("the road cormac mccarthey").as_deref(),
            Some("the road cormac mccarthy")
        );
    }

    #[test]
    fn what_is_typed_right_is_not_corrected() {
        let q = parse("the habit", &KnowsNothing);
        assert_eq!(correct(&q, &shelf()), Verdict::Exact);
        assert_eq!(
            correct(&parse("dune", &KnowsNothing), &shelf()),
            Verdict::Exact
        );
        assert_eq!(fix_of("hitchhikers guide"), None);
        assert_eq!(fix_of("ursula le guin"), None);
        assert_eq!(fix_of("dune 2"), None);
        let q = parse("the god of the woods", &KnowsNothing);
        assert_eq!(
            correct(&q, &[t("God", "Woody Allen", 40)]),
            Verdict::Nothing
        );
    }

    #[test]
    fn near_misses_keep_their_first_letter_and_their_digits() {
        assert_eq!(near("dnue", "dune"), Some(1));
        assert_eq!(near("herbrt", "herbert"), Some(1));
        assert_eq!(near("frankenstien", "frankenstein"), Some(1));
        assert_eq!(near("ry", "rye"), Some(1));
        assert_eq!(near("ry", "dry"), None);
        assert_eq!(
            near("bune", "dune"),
            None,
            "a first letter changed is another word"
        );
        assert_eq!(near("1984", "1985"), None);
        assert_eq!(near("habit", "hobbit"), None, "two edits in a short word");
    }

    #[test]
    fn the_fixed_work_is_known_by_its_id_or_its_title_and_author() {
        let Verdict::Fix(mut f) = correct(&parse("dnue herbrt", &KnowsNothing), &shelf()) else {
            panic!("no fix");
        };
        f.ol_works = vec!["OL893414W".into()];
        assert!(is_fixed_work(&f, Some("/works/OL893414W"), &[], &[]));
        assert!(
            !is_fixed_work(&f, Some("/works/OL893414W"), &["Dune Messiah"], &[]),
            "an id beside another title is not trusted"
        );
        assert!(is_fixed_work(
            &f,
            None,
            &["Dune"],
            &["Frank Herbert".into()]
        ));
        assert!(!is_fixed_work(
            &f,
            None,
            &["Dune"],
            &["Somebody Else".into()]
        ));
        assert!(!is_fixed_work(
            &f,
            None,
            &["Dune Messiah"],
            &["Frank Herbert".into()]
        ));
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::search::query::{KnowsNothing, parse};
    use proptest::prelude::*;

    fn title() -> impl Strategy<Value = String> {
        "[a-z]{3,9}( [a-z]{2,9}){0,3}"
    }

    proptest! {
        #[test]
        fn an_exact_title_is_never_corrected(
            typed in title(),
            others in proptest::collection::vec((title(), "[a-z]{3,8} [a-z]{3,8}"), 0..12),
        ) {
            let mut targets: Vec<Target> = others
                .into_iter()
                .map(|(t, a)| Target { title: t, author: Some(a), ol_works: vec![], notability: Some(5) })
                .collect();
            targets.push(Target { title: typed.clone(), author: None, ol_works: vec![], notability: Some(1) });
            let q = parse(&typed, &KnowsNothing);
            prop_assert!(!matches!(correct(&q, &targets), Verdict::Fix(_)));
        }

        #[test]
        fn a_fix_replaces_words_and_keeps_the_rest(
            typed in title(),
            others in proptest::collection::vec((title(), "[a-z]{3,8} [a-z]{3,8}"), 1..12),
        ) {
            let targets: Vec<Target> = others
                .into_iter()
                .map(|(t, a)| Target { title: t, author: Some(a), ol_works: vec![], notability: Some(5) })
                .collect();
            let q = parse(&typed, &KnowsNothing);
            if let Verdict::Fix(f) = correct(&q, &targets) {
                let out: Vec<&str> = f.searched.split(' ').collect();
                prop_assert_eq!(out.len(), q.words.len());
                let changed = out.iter().zip(&q.words).filter(|(a, b)| **a != b.as_str()).count();
                prop_assert_eq!(changed, f.typos);
                prop_assert!(f.typos >= 1 && f.typos <= MAX_TYPOS);
            }
        }
    }
}
