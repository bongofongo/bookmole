//! Which volume of a work a title names: titles with different volume numbers are different books,
//! however many words they share.

use unicode_normalization::UnicodeNormalization;

use crate::search::query::fold;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Division {
    Volume,
    Part,
    Book,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Volumes(Vec<(Division, u32)>);

/// Division words, folded (lowercase, marks dropped). An ordinary word like `part` is harmless: it
/// is a mark only with a number after it.
const DIVISIONS: &[(&str, Division)] = &[
    ("volume", Division::Volume),
    ("vol", Division::Volume),
    ("tome", Division::Volume),
    ("tomo", Division::Volume),
    ("tom", Division::Volume),
    ("volumen", Division::Volume),
    ("band", Division::Volume),
    ("bd", Division::Volume),
    ("bind", Division::Volume),
    ("svazek", Division::Volume),
    ("τομος", Division::Volume),
    ("том", Division::Volume),
    ("part", Division::Part),
    ("pt", Division::Part),
    ("partie", Division::Part),
    ("parte", Division::Part),
    ("teil", Division::Part),
    ("deel", Division::Part),
    ("del", Division::Part),
    ("dil", Division::Part),
    ("czesc", Division::Part),
    ("pars", Division::Part),
    ("μερος", Division::Part),
    ("часть", Division::Part),
    ("book", Division::Book),
    ("bk", Division::Book),
    ("livre", Division::Book),
    ("libro", Division::Book),
    ("livro", Division::Book),
    ("buch", Division::Book),
    ("boek", Division::Book),
    ("bok", Division::Book),
    ("liber", Division::Book),
    ("ksiega", Division::Book),
    ("βιβλιο", Division::Book),
    ("книга", Division::Book),
];

const CARDINALS: &[(&str, u32)] = &[
    ("one", 1),
    ("two", 2),
    ("three", 3),
    ("four", 4),
    ("five", 5),
    ("six", 6),
    ("seven", 7),
    ("eight", 8),
    ("nine", 9),
    ("ten", 10),
    ("eleven", 11),
    ("twelve", 12),
    ("un", 1),
    ("une", 1),
    ("deux", 2),
    ("trois", 3),
    ("quatre", 4),
    ("cinq", 5),
    ("eins", 1),
    ("zwei", 2),
    ("drei", 3),
    ("vier", 4),
    ("funf", 5),
    ("uno", 1),
    ("una", 1),
    ("dos", 2),
    ("tres", 3),
    ("cuatro", 4),
    ("cinco", 5),
    ("due", 2),
    ("tre", 3),
];

const ORDINALS: &[(&str, u32)] = &[
    ("first", 1),
    ("second", 2),
    ("third", 3),
    ("fourth", 4),
    ("fifth", 5),
    ("sixth", 6),
    ("seventh", 7),
    ("eighth", 8),
    ("ninth", 9),
    ("tenth", 10),
    ("eleventh", 11),
    ("twelfth", 12),
    ("premier", 1),
    ("premiere", 1),
    ("seconde", 2),
    ("deuxieme", 2),
    ("troisieme", 3),
    ("quatrieme", 4),
    ("cinquieme", 5),
    ("primero", 1),
    ("primera", 1),
    ("segundo", 2),
    ("segunda", 2),
    ("tercero", 3),
    ("tercera", 3),
    ("cuarto", 4),
    ("cuarta", 4),
    ("quinto", 5),
    ("quinta", 5),
    ("primo", 1),
    ("prima", 1),
    ("secondo", 2),
    ("seconda", 2),
    ("terzo", 3),
    ("terza", 3),
    ("первыи", 1),
    ("первая", 1),
    ("второи", 2),
    ("вторая", 2),
    ("третии", 3),
    ("третья", 3),
    ("πρωτος", 1),
    ("πρωτο", 1),
    ("δευτερος", 2),
    ("δευτερο", 2),
    ("τριτος", 3),
    ("τριτο", 3),
];

/// German ordinal stems; a bare stem is not read (`acht` alone is the cardinal eight), it needs one
/// of `GERMAN_ENDINGS`.
const GERMAN_STEMS: &[(&str, u32)] = &[
    ("erst", 1),
    ("zweit", 2),
    ("dritt", 3),
    ("viert", 4),
    ("funft", 5),
    ("sechst", 6),
    ("siebt", 7),
    ("acht", 8),
    ("neunt", 9),
    ("zehnt", 10),
];
const GERMAN_ENDINGS: &[&str] = &["er", "es", "en", "em", "e"];

const DIGIT_ORDINAL_SUFFIXES: &[&str] = &["st", "nd", "rd", "th", "e", "er", "re", "eme", "o", "a"];

const BETWEEN: &[&str] = &["the", "no", "nr", "number"];

/// CJK units, and whether the number needs a leading `第`: a bare `一部` is *a portion*, not a volume
/// mark.
const CJK_UNITS: &[(char, Division, bool)] = &[
    ('巻', Division::Volume, false),
    ('卷', Division::Volume, false),
    ('冊', Division::Volume, false),
    ('册', Division::Volume, false),
    ('권', Division::Volume, false),
    ('集', Division::Volume, true),
    ('部', Division::Part, true),
    ('篇', Division::Part, true),
];

/// `下巻` is the last volume: a number no counted volume has, equal only to itself and never claimed
/// to be 2.
const LAST_VOLUME: u32 = 10_000;

impl Volumes {
    pub(crate) fn of(title: &str) -> Self {
        let folded = fold(title);
        let toks: Vec<&str> = folded.split_whitespace().collect();
        let mut marks = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            if let Some(div) = division(toks[i]) {
                let mut j = i + 1;
                if toks.get(j).is_some_and(|t| BETWEEN.contains(t)) {
                    j += 1;
                }
                if let Some(n) = toks.get(j).and_then(|t| number(t)) {
                    marks.push((div, n));
                    i = j + 1;
                    continue;
                }
            }
            //  One step, not two: the division word is read again, so `first volume 3` keeps both the 1 and the
            //  3.
            if let Some(n) = ordinal(toks[i])
                && let Some(div) = toks.get(i + 1).and_then(|t| division(t))
            {
                marks.push((div, n));
            }
            i += 1;
        }
        //  Not the folded string: NFKD splits Hangul into jamo; NFKC keeps it whole and still reads
        //  full-width digits.
        cjk_marks(&title.nfkc().collect::<String>(), &mut marks);
        marks.sort();
        marks.dedup();
        Volumes(marks)
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Different volumes means certainly different books. Symmetric, and never true of a title against
    /// itself.
    pub(crate) fn differ(&self, other: &Volumes) -> bool {
        if self.0.is_empty() || other.0.is_empty() {
            return false;
        }
        let shared: Vec<Division> = self
            .divisions()
            .filter(|d| other.divisions().any(|e| e == *d))
            .collect();
        if shared.is_empty() {
            return !self
                .0
                .iter()
                .any(|(_, n)| other.0.iter().any(|(_, m)| m == n));
        }
        shared
            .iter()
            .any(|d| self.numbers(*d).ne(other.numbers(*d)))
    }

    fn divisions(&self) -> impl Iterator<Item = Division> + '_ {
        let mut seen: Vec<Division> = self.0.iter().map(|(d, _)| *d).collect();
        seen.dedup();
        seen.into_iter()
    }

    fn numbers(&self, d: Division) -> impl Iterator<Item = u32> + '_ {
        self.0.iter().filter(move |(e, _)| *e == d).map(|(_, n)| *n)
    }
}

fn division(tok: &str) -> Option<Division> {
    DIVISIONS
        .iter()
        .find(|(w, _)| same(w, tok))
        .map(|(_, d)| *d)
}

/// Compares folded words with Greek final sigma read as sigma (capitals fold to `σ`, lowercase to
/// `ς`).
fn same(a: &str, b: &str) -> bool {
    let sigma = |c: char| if c == 'ς' { 'σ' } else { c };
    a.chars().map(sigma).eq(b.chars().map(sigma))
}

fn number(tok: &str) -> Option<u32> {
    digits(tok)
        .or_else(|| roman(tok))
        .or_else(|| CARDINALS.iter().find(|(w, _)| *w == tok).map(|(_, n)| *n))
        .or_else(|| ordinal(tok))
}

fn ordinal(tok: &str) -> Option<u32> {
    if let Some((_, n)) = ORDINALS.iter().find(|(w, _)| same(w, tok)) {
        return Some(*n);
    }
    for end in GERMAN_ENDINGS {
        if let Some(stem) = tok.strip_suffix(end)
            && let Some((_, n)) = GERMAN_STEMS.iter().find(|(s, _)| *s == stem)
        {
            return Some(*n);
        }
    }
    let split = tok.find(|c: char| !c.is_ascii_digit())?;
    let (d, suffix) = tok.split_at(split);
    if DIGIT_ORDINAL_SUFFIXES.contains(&suffix) {
        digits(d)
    } else {
        None
    }
}

/// At most four digits: stops a long digit run from overflowing.
fn digits(tok: &str) -> Option<u32> {
    if tok.is_empty() || tok.len() > 4 || !tok.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    tok.parse().ok()
}

/// Canonical roman numerals up to 399 only, so `ic`, `iiii` and letter-shuffled words are not
/// numbers.
fn roman(tok: &str) -> Option<u32> {
    if tok.is_empty()
        || !tok
            .chars()
            .all(|c| matches!(c, 'i' | 'v' | 'x' | 'l' | 'c'))
    {
        return None;
    }
    let value = |c: char| match c {
        'i' => 1,
        'v' => 5,
        'x' => 10,
        'l' => 50,
        _ => 100,
    };
    let cs: Vec<i64> = tok.chars().map(value).collect();
    let mut n = 0i64;
    for (k, v) in cs.iter().enumerate() {
        if cs.get(k + 1).is_some_and(|next| next > v) {
            n -= v;
        } else {
            n += v;
        }
    }
    let n = u32::try_from(n).ok()?;
    (n > 0 && n < 400 && to_roman(n) == tok).then_some(n)
}

fn to_roman(mut n: u32) -> String {
    const TABLE: &[(u32, &str)] = &[
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut out = String::new();
    for (v, s) in TABLE {
        while n >= *v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

/// `全3巻` is a count (three volumes in all), not a mark, and is not read.
fn cjk_marks(folded: &str, marks: &mut Vec<(Division, u32)>) {
    let cs: Vec<char> = folded.chars().collect();
    for (k, c) in cs.iter().enumerate() {
        let Some((_, div, needs_di)) = CJK_UNITS.iter().find(|(u, _, _)| u == c) else {
            continue;
        };
        let start = cs[..k]
            .iter()
            .rposition(|c| !is_numeral(*c))
            .map_or(0, |p| p + 1);
        let numerals = &cs[start..k];
        let before = start.checked_sub(1).map(|p| cs[p]);
        if numerals.is_empty() {
            if matches!(c, '巻' | '卷') {
                let n = match before {
                    Some('上') => 1,
                    Some('中') => 2,
                    Some('下') => LAST_VOLUME,
                    _ => continue,
                };
                marks.push((*div, n));
            }
            continue;
        }
        if before == Some('全') || (*needs_di && before != Some('第')) {
            continue;
        }
        if let Some(n) = cjk_number(numerals) {
            marks.push((*div, n));
        }
    }
}

fn is_numeral(c: char) -> bool {
    c.is_ascii_digit() || "〇零一二三四五六七八九十百".contains(c)
}

fn cjk_number(cs: &[char]) -> Option<u32> {
    if cs.iter().all(char::is_ascii_digit) {
        return digits(&cs.iter().collect::<String>());
    }
    let digit = |c: char| "〇一二三四五六七八九".find(c).map(|b| (b / 3) as u32);
    let mut total = 0u32;
    let mut pending: Option<u32> = None;
    for c in cs {
        match c {
            '百' => {
                total += pending.take().unwrap_or(1) * 100;
            }
            '十' => {
                total += pending.take().unwrap_or(1) * 10;
            }
            '零' => {}
            _ => {
                if pending.is_some() {
                    return None;
                }
                pending = Some(digit(*c)?);
            }
        }
    }
    let n = total + pending.unwrap_or(0);
    (n > 0).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(t: &str) -> Vec<(Division, u32)> {
        Volumes::of(t).0
    }

    fn differ(a: &str, b: &str) -> bool {
        Volumes::of(a).differ(&Volumes::of(b))
    }

    #[test]
    fn the_two_sets_the_corpus_found_are_two_volumes_each() {
        assert!(differ(
            "The Works of Edgar Allan Poe — Volume 1",
            "The Works of Edgar Allan Poe — Volume 2"
        ));
        assert!(differ("Κύρου Ανάβασις Τόμος 1", "Κύρου Ανάβασις Τόμος 2"));
        assert!(!differ(
            "Faust: Eine Tragödie [erster Teil]",
            "Faust: Der Tragödie erster Teil"
        ));
        assert!(!differ("Pride and Prejudice", "Pride and Prejudice"));
    }

    #[test]
    fn every_spelling_of_a_number_is_one_number() {
        let one = [
            "Les misérables Tome I: Fantine",
            "Les misérables, tome 1",
            "Les Misérables — Tome premier",
            "Les misérables, Tome un",
        ];
        for t in one {
            assert_eq!(marks(t), [(Division::Volume, 1)], "{t}");
        }
        assert_eq!(marks("Vol. IV"), [(Division::Volume, 4)]);
        assert_eq!(marks("Book the Second"), [(Division::Book, 2)]);
        assert_eq!(marks("Part Two"), [(Division::Part, 2)]);
        assert_eq!(marks("The 3rd Book"), [(Division::Book, 3)]);
        assert_eq!(marks("Faust. Zweiter Teil"), [(Division::Part, 2)]);
        assert_eq!(marks("Ksiega pierwsza Część 2"), [(Division::Part, 2)]);
        assert_eq!(marks("Вторая книга"), [(Division::Book, 2)]);
        assert_eq!(marks("Война и мир. Том 3"), [(Division::Volume, 3)]);
        assert_eq!(marks("Band 12"), [(Division::Volume, 12)]);
        assert_eq!(marks("Vol. No. 7"), [(Division::Volume, 7)]);
    }

    #[test]
    fn the_cjk_forms_are_read_and_a_count_is_not() {
        assert_eq!(marks("源氏物語 第2巻"), [(Division::Volume, 2)]);
        assert_eq!(marks("源氏物語第二巻"), [(Division::Volume, 2)]);
        assert_eq!(marks("三国志 第十二卷"), [(Division::Volume, 12)]);
        assert_eq!(marks("鋼の錬金術師 3巻"), [(Division::Volume, 3)]);
        assert_eq!(marks("第一部 風の章"), [(Division::Part, 1)]);
        assert_eq!(marks("채식주의자 2권"), [(Division::Volume, 2)]);
        assert!(differ("細雪 上巻", "細雪 下巻"));
        assert!(!differ("細雪 下巻", "細雪 下巻"));
        assert!(marks("一部の人").is_empty());
        assert!(marks("源氏物語 全3巻").is_empty());
    }

    #[test]
    fn a_word_that_is_only_sometimes_a_division_is_a_mark_only_with_a_number() {
        for t in [
            "The Jungle Book",
            "The Book of Household Management",
            "Part of the Problem",
            "The Second Jungle Book",
            "War and Peace",
            "A Novel in Three Volumes",
            "Uncle Tom's Cabin",
            "Dune 2",
            "Ivanhoe",
        ] {
            assert!(Volumes::of(t).is_empty(), "{t}: {:?}", marks(t));
        }
    }

    #[test]
    fn a_title_naming_no_volume_disagrees_with_nothing() {
        assert!(!differ(
            "Harry Potter and the Philosopher's Stone",
            "Harry Potter and the Philosopher's Stone (Harry Potter, Book 1)"
        ));
        assert!(!differ("Les Misérables", "Les misérables Tome II: Cosette"));
    }

    #[test]
    fn divisions_are_compared_with_their_own_kind() {
        assert!(differ("Volume 1, Part 2", "Volume 1, Part 3"));
        assert!(!differ("Volume 1, Part 2", "Part 2"));
        assert!(!differ("Tome I", "Book 1"));
        assert!(differ("Volume 2", "Book 1"));
        assert!(differ(
            "Harry Potter and the Chamber of Secrets (Harry Potter, Book 2)",
            "Harry Potter and the Philosopher's Stone (Harry Potter, Book 1)"
        ));
        assert!(differ(
            "The First Part of King Henry the Fourth",
            "The Second Part of King Henry the Fourth"
        ));
    }

    #[test]
    fn roman_numerals_are_canonical_only() {
        assert_eq!(roman("iv"), Some(4));
        assert_eq!(roman("xiv"), Some(14));
        assert_eq!(roman("cxcix"), Some(199));
        assert_eq!(roman("iiii"), None);
        assert_eq!(roman("ic"), None);
        assert_eq!(roman("civil"), None);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn spellings(n: u32) -> Vec<String> {
            let mut out = vec![n.to_string(), to_roman(n)];
            if let Some((w, _)) = CARDINALS.iter().find(|(_, m)| *m == n) {
                out.push((*w).to_string());
            }
            if let Some((w, _)) = ORDINALS.iter().find(|(_, m)| *m == n) {
                out.push((*w).to_string());
            }
            out
        }

        fn words_of(d: Division) -> Vec<&'static str> {
            DIVISIONS
                .iter()
                .filter(|(_, e)| *e == d)
                .map(|(w, _)| *w)
                .collect()
        }

        fn division() -> impl Strategy<Value = Division> {
            prop_oneof![
                Just(Division::Volume),
                Just(Division::Part),
                Just(Division::Book)
            ]
        }

        proptest! {
            #[test]
            fn a_title_never_differs_from_itself(t in "\\PC{0,60}") {
                prop_assert!(!Volumes::of(&t).differ(&Volumes::of(&t)));
            }

            #[test]
            fn differing_is_symmetric(a in "\\PC{0,40}", b in "\\PC{0,40}") {
                let (va, vb) = (Volumes::of(&a), Volumes::of(&b));
                prop_assert_eq!(va.differ(&vb), vb.differ(&va));
            }

            #[test]
            fn the_spelling_never_decides_and_the_number_always_does(
                base in "[A-Za-z]{3,12}( [A-Za-z]{3,12}){0,3}",
                d in division(),
                n in 1u32..=12,
                m in 1u32..=12,
                pick in any::<[prop::sample::Index; 4]>(),
            ) {
                let words = words_of(d);
                let (wa, wb) = (pick[0].get(&words), pick[1].get(&words));
                let (sn, sm) = (spellings(n), spellings(m));
                let (na, nb) = (pick[2].get(&sn), pick[3].get(&sn));
                let a = format!("{base} {wa} {na}");
                let b = format!("{base} {wb} {nb}");
                prop_assume!(Volumes::of(&base).is_empty());
                prop_assert!(!Volumes::of(&a).differ(&Volumes::of(&b)), "{a} / {b}");
                let c = format!("{base} {wb} {}", pick[3].get(&sm));
                prop_assert_eq!(Volumes::of(&a).differ(&Volumes::of(&c)), n != m, "{} / {}", a, c);
            }
        }
    }
}
