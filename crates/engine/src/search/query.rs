//! One pure parse of free text into a `ParsedQuery`: folded text, any ISBN, weighted readings.
//! Its fold is not `super::normalize`, which defines "same title" for imports; this is for ranking.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::book::normalize_isbn;
use crate::providers::normalize_language;

/// NFKD, marks dropped, lowercased, other non-alphanumerics a space. An apostrophe joins except
/// after a single letter, where it splits (`l'écume` -> `l ecume`). Idempotent.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut word_len = 0usize;
    for c in s.nfkd() {
        if is_combining_mark(c) {
            continue;
        }
        let spelled: Option<&str> = match c {
            'æ' | 'Æ' => Some("ae"),
            'œ' | 'Œ' => Some("oe"),
            'ß' | 'ẞ' => Some("ss"),
            'ø' | 'Ø' => Some("o"),
            'ł' | 'Ł' => Some("l"),
            'đ' | 'Đ' => Some("d"),
            'þ' | 'Þ' => Some("th"),
            'ı' => Some("i"),
            _ => None,
        };
        if let Some(sp) = spelled {
            out.push_str(sp);
            word_len += sp.len();
            continue;
        }
        if is_apostrophe(c) {
            if word_len == 1 {
                out.push(' ');
                word_len = 0;
            }
            continue;
        }
        if c.is_alphanumeric() {
            for l in c.to_lowercase() {
                out.push(l);
            }
            word_len += 1;
        } else {
            out.push(' ');
            word_len = 0;
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_apostrophe(c: char) -> bool {
    matches!(c, '\'' | '’' | '‘' | 'ʼ' | '′' | '`' | '´' | '‛')
}

fn plain_punctuation(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '“' | '”' | '„' | '«' | '»' => '"',
            c if is_apostrophe(c) => '\'',
            c if c.is_whitespace() => ' ',
            c => c,
        })
        .collect()
}

/// `i` is left out: Italian's plural article and English's pronoun; *I, Robot* is a title.
const ARTICLES: &[&str] = &[
    "the", "a", "an", "le", "la", "les", "l", "un", "une", "der", "die", "das", "ein", "eine",
    "el", "los", "las", "una", "il", "lo", "gli", "uno", "o", "os", "as", "um", "uma", "de", "het",
    "een", "en", "ett", "den", "det",
];

/// A title that is nothing but an article keeps it.
pub fn title_key(folded: &str) -> &str {
    match folded.split_once(' ') {
        Some((first, rest)) if ARTICLES.contains(&first) && !rest.is_empty() => rest,
        _ => folded,
    }
}

pub(crate) fn is_filler(word: &str) -> bool {
    ARTICLES.contains(&word)
        || matches!(
            word,
            "of" | "and"
                | "or"
                | "in"
                | "on"
                | "to"
                | "for"
                | "from"
                | "with"
                | "at"
                | "by"
                | "is"
                | "du"
                | "des"
                | "et"
                | "y"
                | "e"
                | "und"
                | "von"
                | "del"
                | "di"
                | "da"
        )
}

pub trait KnownNames {
    fn knows_author(&self, folded: &str) -> bool;

    fn knows_whole_author(&self, _folded: &str) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct KnowsNothing;

impl KnownNames for KnowsNothing {
    fn knows_author(&self, _: &str) -> bool {
        false
    }
}

impl<F: Fn(&str) -> bool> KnownNames for F {
    fn knows_author(&self, folded: &str) -> bool {
        self(folded)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReadingKind {
    Isbn,
    Fielded,
    TitleByAuthor,
    Dashed,
    Comma,
    WholeTitle,
    Split,
    AuthorOnly,
    Series,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub kind: ReadingKind,
    pub title: Option<String>,
    pub author: Option<String>,
    pub series: Option<String>,
    pub position: Option<f64>,
    /// `0.0..=1.0`; ranking multiplies a result's score under this reading by it.
    pub weight: f64,
}

impl Reading {
    fn new(kind: ReadingKind, weight: f64) -> Self {
        Reading {
            kind,
            title: None,
            author: None,
            series: None,
            position: None,
            weight,
        }
    }
    fn title(mut self, t: impl Into<String>) -> Self {
        self.title = Some(t.into()).filter(|t: &String| !t.is_empty());
        self
    }
    fn author(mut self, a: impl Into<String>) -> Self {
        self.author = Some(a.into()).filter(|a: &String| !a.is_empty());
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedQuery {
    /// What was typed, trimmed. Never logged above `trace!`.
    pub text: String,
    pub folded: String,
    pub words: Vec<String>,
    pub isbn: Option<String>,
    pub not_an_isbn: bool,
    pub lang: Option<String>,
    /// As typed, not folded, so providers get the user's own spelling.
    pub fields: Fields,
    pub readings: Vec<Reading>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fields {
    pub title: Option<String>,
    pub author: Option<String>,
    pub series: Option<String>,
    pub free: Option<String>,
}

impl ParsedQuery {
    pub fn reading(&self, kind: ReadingKind) -> Option<&Reading> {
        self.readings
            .iter()
            .filter(|r| r.kind == kind)
            .max_by(|a, b| a.weight.total_cmp(&b.weight))
    }

    pub fn marked(&self) -> Option<&Reading> {
        self.readings
            .iter()
            .filter(|r| {
                matches!(
                    r.kind,
                    ReadingKind::TitleByAuthor | ReadingKind::Dashed | ReadingKind::Comma
                ) && r.title.is_some()
                    && r.author.is_some()
            })
            .max_by(|a, b| a.weight.total_cmp(&b.weight))
    }
}

// A marker someone typed (`by`, a field) is evidence; an unmarked split is a guess until a known
// name backs it.
const W_MARKED: f64 = 1.0;
const W_MARKED_OTHER_WAY: f64 = 0.9;
const W_MARKED_AS_TITLE: f64 = 0.9;
const W_WHOLE: f64 = 1.0;
const W_SPLIT: f64 = 0.8;
const W_SPLIT_KNOWN: f64 = 1.0;
const W_AUTHOR_ONLY: f64 = 0.75;
const W_AUTHOR_ONLY_KNOWN: f64 = 1.0;
const W_SERIES: f64 = 0.95;

/// Longer texts are not split every way: a pasted sentence is a title, and its readings are noise.
const MAX_SPLIT_WORDS: usize = 10;

pub fn parse(text: &str, known: &dyn KnownNames) -> ParsedQuery {
    let typed = text.trim().to_string();
    let plain = plain_punctuation(&typed);
    let folded = fold(&plain);
    let words: Vec<String> = folded.split_whitespace().map(str::to_string).collect();
    let mut q = ParsedQuery {
        text: typed,
        folded,
        words,
        isbn: None,
        not_an_isbn: false,
        lang: None,
        fields: Fields::default(),
        readings: Vec::new(),
    };

    if let Some(fields) = fielded(&plain) {
        read_fields(&mut q, fields);
        return q;
    }

    match isbn_of(&plain) {
        IsbnShape::Valid(isbn) => {
            q.isbn = Some(isbn);
            q.readings.push(Reading::new(ReadingKind::Isbn, 1.0));
            return q;
        }
        IsbnShape::Invalid => q.not_an_isbn = true,
        IsbnShape::NotOne => {}
    }

    let (marked, typed) = marked_readings(&plain);
    let was_marked = !marked.is_empty();
    q.readings.extend(marked);
    if let Some((title, author)) = typed {
        q.fields.title = Some(title);
        q.fields.author = Some(author);
    }

    // Every query is also tried as a whole title: a marker can be part of one (*Stand by Me*).
    let whole_weight = if was_marked {
        W_MARKED_AS_TITLE
    } else {
        W_WHOLE
    };
    if !q.folded.is_empty() {
        q.readings
            .push(Reading::new(ReadingKind::WholeTitle, whole_weight).title(q.folded.clone()));
    }

    if !was_marked {
        let n = q.words.len();
        if (2..=MAX_SPLIT_WORDS).contains(&n) {
            for k in 1..n {
                let head = q.words[..k].join(" ");
                let tail = q.words[k..].join(" ");
                for (title, name) in [(&head, &tail), (&tail, &head)] {
                    if name.split(' ').all(is_filler) || title.split(' ').all(is_filler) {
                        continue;
                    }
                    let w = if known.knows_author(name) {
                        W_SPLIT_KNOWN
                    } else {
                        W_SPLIT
                    };
                    q.readings.push(
                        Reading::new(ReadingKind::Split, w)
                            .title(title.clone())
                            .author(name.clone()),
                    );
                }
            }
        }
        if n >= 1 && !q.words.iter().all(|w| is_filler(w)) {
            // Believed no more than the title: *Tom Lake* is a novel and a man; edition count separates them.
            let w = if known.knows_whole_author(&q.folded) {
                W_AUTHOR_ONLY_KNOWN
            } else {
                W_AUTHOR_ONLY
            };
            q.readings
                .push(Reading::new(ReadingKind::AuthorOnly, w).author(q.folded.clone()));
        }
        if let Some((series, position)) = series_of(&q.words) {
            let mut r = Reading::new(ReadingKind::Series, W_SERIES);
            r.series = Some(series);
            r.position = Some(position);
            q.readings.push(r);
        }
    }
    q
}

enum IsbnShape {
    Valid(String),
    Invalid,
    NotOne,
}

fn isbn_of(plain: &str) -> IsbnShape {
    let mut s = plain.trim();
    let lower = s.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("isbn") {
        let skip = s.len() - rest.len();
        s = s[skip..].trim_start();
        for tag in ["-13", "-10", "13", "10"] {
            if s.starts_with(tag) && s[tag.len()..].starts_with([':', ' ']) {
                s = &s[tag.len()..];
                break;
            }
        }
        s = s.trim_start_matches([':', ' ']);
    }
    if let Some(isbn) = normalize_isbn(s) {
        return IsbnShape::Valid(isbn);
    }
    let compact: String = s.chars().filter(|c| !matches!(c, '-' | ' ')).collect();
    let shaped = matches!(compact.len(), 10 | 13)
        && compact
            .chars()
            .enumerate()
            .all(|(i, c)| c.is_ascii_digit() || (i == 9 && matches!(c, 'x' | 'X')));
    if shaped {
        IsbnShape::Invalid
    } else {
        IsbnShape::NotOne
    }
}

fn marked_readings(plain: &str) -> (Vec<Reading>, Option<(String, String)>) {
    let mut out = Vec::new();
    let mut typed = None;
    // ASCII lowercasing keeps byte offsets, so an index found here slices `plain`.
    let lower = plain.to_ascii_lowercase();

    let bys: Vec<usize> = lower.match_indices(" by ").map(|(i, _)| i).collect();
    for (n, &at) in bys.iter().enumerate() {
        let title = fold(&plain[..at]);
        let author = fold(&plain[at + 4..]);
        if title.is_empty() || author.is_empty() {
            continue;
        }
        let w = if n + 1 == bys.len() {
            typed = Some((
                plain[..at].trim().to_string(),
                plain[at + 4..].trim().to_string(),
            ));
            W_MARKED
        } else {
            W_MARKED_OTHER_WAY
        };
        out.push(
            Reading::new(ReadingKind::TitleByAuthor, w)
                .title(title)
                .author(author),
        );
    }
    if !out.is_empty() {
        return (out, typed);
    }

    if let Some(at) = plain.find(" - ") {
        let (a, b) = (fold(&plain[..at]), fold(&plain[at + 3..]));
        if !a.is_empty() && !b.is_empty() {
            typed = Some((
                plain[..at].trim().to_string(),
                plain[at + 3..].trim().to_string(),
            ));
            out.push(
                Reading::new(ReadingKind::Dashed, W_MARKED)
                    .title(a.clone())
                    .author(b.clone()),
            );
            out.push(
                Reading::new(ReadingKind::Dashed, W_MARKED_OTHER_WAY)
                    .title(b)
                    .author(a),
            );
            return (out, typed);
        }
    }

    // One comma only; more is a title with commas in it.
    if plain.matches(',').count() == 1 {
        let at = plain.find(',').unwrap_or_default();
        let (a, b) = (fold(&plain[..at]), fold(&plain[at + 1..]));
        if !a.is_empty() && !b.is_empty() {
            typed = Some((
                plain[at + 1..].trim().to_string(),
                plain[..at].trim().to_string(),
            ));
            out.push(
                Reading::new(ReadingKind::Comma, W_MARKED)
                    .title(b.clone())
                    .author(a.clone()),
            );
            out.push(
                Reading::new(ReadingKind::Comma, W_MARKED_OTHER_WAY)
                    .title(a)
                    .author(b),
            );
        }
    }
    (out, typed)
}

fn series_of(words: &[String]) -> Option<(String, f64)> {
    let (last, rest) = words.split_last()?;
    let position = number_of(last)?;
    let rest = match rest.split_last() {
        Some((w, before))
            if matches!(
                w.as_str(),
                "book" | "vol" | "volume" | "part" | "no" | "tome"
            ) =>
        {
            before
        }
        _ => rest,
    };
    if rest.is_empty() || rest.iter().all(|w| is_filler(w)) {
        return None;
    }
    Some((rest.join(" "), position))
}

fn number_of(word: &str) -> Option<f64> {
    if let Ok(n) = word.parse::<u32>() {
        return (1..=99).contains(&n).then_some(f64::from(n));
    }
    const WORDS: [&str; 12] = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve",
    ];
    const ORDINALS: [&str; 12] = [
        "first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth",
        "tenth", "eleventh", "twelfth",
    ];
    WORDS
        .iter()
        .position(|w| *w == word)
        .or_else(|| ORDINALS.iter().position(|w| *w == word))
        .map(|i| (i + 1) as f64)
}

/// `None` when no field is named. Only these names are fields: `sapiens: a brief history` is text.
fn fielded(plain: &str) -> Option<Vec<(Option<&'static str>, String)>> {
    const NAMES: [(&str, &str); 6] = [
        ("title", "title"),
        ("author", "author"),
        ("isbn", "isbn"),
        ("series", "series"),
        ("lang", "lang"),
        ("language", "lang"),
    ];
    let lower = plain.to_ascii_lowercase();
    let mut marks: Vec<(usize, usize, &'static str)> = Vec::new();
    for (name, canon) in NAMES {
        let pat = format!("{name}:");
        for (at, _) in lower.match_indices(&pat) {
            let starts_word = at == 0 || lower[..at].ends_with(' ');
            if starts_word {
                marks.push((at, pat.len(), canon));
            }
        }
    }
    if marks.is_empty() {
        return None;
    }
    marks.sort_by_key(|m| m.0);
    let mut parts = Vec::new();
    let free = plain[..marks[0].0].trim();
    if !free.is_empty() {
        parts.push((None, free.to_string()));
    }
    for (i, &(at, len, canon)) in marks.iter().enumerate() {
        let end = marks.get(i + 1).map_or(plain.len(), |m| m.0);
        let value = plain[at + len..end].trim().trim_matches('"').trim();
        if !value.is_empty() {
            parts.push((Some(canon), value.to_string()));
        }
    }
    Some(parts)
}

fn read_fields(q: &mut ParsedQuery, parts: Vec<(Option<&'static str>, String)>) {
    let mut f = Fields::default();
    let mut isbn_text: Option<String> = None;
    for (name, value) in parts {
        let slot = match name {
            Some("title") => &mut f.title,
            Some("author") => &mut f.author,
            Some("series") => &mut f.series,
            Some("isbn") => &mut isbn_text,
            Some("lang") => {
                q.lang = Some(normalize_language(&value));
                continue;
            }
            _ => &mut f.free,
        };
        *slot = Some(match slot.take() {
            Some(was) => format!("{was} {value}"),
            None => value,
        });
    }
    if let Some(raw) = isbn_text {
        match isbn_of(&raw) {
            IsbnShape::Valid(isbn) => q.isbn = Some(isbn),
            _ => {
                q.not_an_isbn = true;
                f.free = Some(match f.free.take() {
                    Some(was) => format!("{was} {raw}"),
                    None => raw,
                });
            }
        }
    }
    if q.isbn.is_some() {
        q.readings.push(Reading::new(ReadingKind::Isbn, 1.0));
    }
    let title = f.title.as_deref().or(f.free.as_deref()).map(fold);
    let mut r = Reading::new(ReadingKind::Fielded, 1.0);
    r.title = title.filter(|t| !t.is_empty());
    r.author = f.author.as_deref().map(fold).filter(|a| !a.is_empty());
    r.series = f.series.as_deref().map(fold).filter(|s| !s.is_empty());
    if r.title.is_some() || r.author.is_some() || r.series.is_some() {
        q.readings.push(r);
    }
    q.fields = f;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(q: &ParsedQuery) -> Vec<ReadingKind> {
        q.readings.iter().map(|r| r.kind).collect()
    }

    fn has(q: &ParsedQuery, kind: ReadingKind, title: Option<&str>, author: Option<&str>) -> bool {
        q.readings
            .iter()
            .any(|r| r.kind == kind && r.title.as_deref() == title && r.author.as_deref() == author)
    }

    #[test]
    fn folding_takes_marks_ligatures_quotes_and_case() {
        assert_eq!(fold("Cien años de soledad"), "cien anos de soledad");
        assert_eq!(fold("Œuvres complètes"), "oeuvres completes");
        assert_eq!(fold("Straße"), "strasse");
        assert_eq!(fold("ﬁnal"), "final");
        assert_eq!(fold("Bolesław"), "boleslaw");
        assert_eq!(fold("The Hitchhiker’s Guide"), "the hitchhikers guide");
        assert_eq!(fold("L’Écume des jours"), "l ecume des jours");
        assert_eq!(fold("Slaughterhouse–Five"), "slaughterhouse five");
        assert_eq!(fold("  “Quoted”  "), "quoted");
    }

    #[test]
    fn the_article_list_is_for_ranking_and_wider_than_matching_s() {
        assert_eq!(title_key("the hobbit"), "hobbit");
        assert_eq!(title_key("le petit prince"), "petit prince");
        assert_eq!(title_key("l ecume des jours"), "ecume des jours");
        assert_eq!(title_key("der zauberberg"), "zauberberg");
        assert_eq!(title_key("the"), "the", "a title of one article keeps it");
        assert_eq!(title_key("i robot"), "i robot");
        assert_eq!(
            super::super::normalize("Le Petit Prince"),
            "le petit prince"
        );
    }

    #[test]
    fn an_isbn_in_any_spacing_is_one_isbn() {
        for (typed, want) in [
            ("9780441172719", "9780441172719"),
            ("978-0-441-17271-9", "9780441172719"),
            ("978 0 441 17271 9", "9780441172719"),
            ("0441172717", "0441172717"),
            ("0 441 17271 7", "0441172717"),
            ("0-441-17271-7", "0441172717"),
            ("ISBN 978-0-441-17271-9", "9780441172719"),
            ("isbn: 0441172717", "0441172717"),
            ("ISBN-13: 978–0–441–17271–9", "9780441172719"),
        ] {
            let q = parse(typed, &KnowsNothing);
            assert_eq!(q.isbn.as_deref(), Some(want), "{typed:?}");
            assert_eq!(normalize_isbn(want).as_deref(), Some(want));
            assert_eq!(kinds(&q), vec![ReadingKind::Isbn]);
        }
    }

    #[test]
    fn a_bad_checksum_is_searched_as_text_and_says_so() {
        let q = parse("9780441172718", &KnowsNothing);
        assert_eq!(q.isbn, None);
        assert!(q.not_an_isbn);
        assert!(has(
            &q,
            ReadingKind::WholeTitle,
            Some("9780441172718"),
            None
        ));
        assert!(!parse("1984", &KnowsNothing).not_an_isbn);
    }

    #[test]
    fn title_by_author_and_its_cousins() {
        let q = parse("dune by frank herbert", &KnowsNothing);
        assert!(has(
            &q,
            ReadingKind::TitleByAuthor,
            Some("dune"),
            Some("frank herbert")
        ));
        assert!(has(
            &q,
            ReadingKind::WholeTitle,
            Some("dune by frank herbert"),
            None
        ));
        assert_eq!(
            q.marked()
                .map(|r| (r.title.as_deref(), r.author.as_deref())),
            Some((Some("dune"), Some("frank herbert")))
        );

        let q = parse("the road – mccarthy", &KnowsNothing);
        assert_eq!(
            q.marked()
                .map(|r| (r.title.as_deref(), r.author.as_deref())),
            Some((Some("the road"), Some("mccarthy")))
        );

        let q = parse("herbert, dune", &KnowsNothing);
        assert_eq!(
            q.marked()
                .map(|r| (r.title.as_deref(), r.author.as_deref())),
            Some((Some("dune"), Some("herbert")))
        );
        assert!(
            parse("eats, shoots, and leaves", &KnowsNothing)
                .marked()
                .is_none()
        );
    }

    #[test]
    fn a_by_inside_a_title_does_not_hide_it() {
        let q = parse("stand by me", &KnowsNothing);
        assert!(has(&q, ReadingKind::WholeTitle, Some("stand by me"), None));
    }

    #[test]
    fn unmarked_text_is_tried_every_way() {
        let q = parse("dune herbert", &KnowsNothing);
        assert!(has(&q, ReadingKind::WholeTitle, Some("dune herbert"), None));
        assert!(has(&q, ReadingKind::Split, Some("dune"), Some("herbert")));
        assert!(has(&q, ReadingKind::Split, Some("herbert"), Some("dune")));
        assert!(has(&q, ReadingKind::AuthorOnly, None, Some("dune herbert")));
        let split = q
            .readings
            .iter()
            .find(|r| r.kind == ReadingKind::Split && r.author.as_deref() == Some("herbert"))
            .unwrap();
        assert!(split.weight < W_WHOLE, "an unbacked split is a guess");

        let known = |n: &str| n == "herbert";
        let q = parse("dune herbert", &known);
        let split = q
            .readings
            .iter()
            .find(|r| r.kind == ReadingKind::Split && r.author.as_deref() == Some("herbert"))
            .unwrap();
        assert_eq!(split.weight, W_SPLIT_KNOWN);
    }

    #[test]
    fn a_whole_known_name_is_believed_as_an_author() {
        struct Names;
        impl KnownNames for Names {
            fn knows_author(&self, f: &str) -> bool {
                f == "james" || f == "haruki murakami"
            }
            fn knows_whole_author(&self, f: &str) -> bool {
                f == "haruki murakami"
            }
        }
        let q = parse("haruki murakami", &Names);
        let author = q.reading(ReadingKind::AuthorOnly).unwrap().weight;
        let title = q.reading(ReadingKind::WholeTitle).unwrap().weight;
        assert_eq!(author, title, "a name and a title, equally");

        let q = parse("james", &Names);
        let author = q.reading(ReadingKind::AuthorOnly).unwrap().weight;
        let title = q.reading(ReadingKind::WholeTitle).unwrap().weight;
        assert!(title > author, "a first name is not somebody");
    }

    #[test]
    fn a_series_and_its_place() {
        for (typed, series, pos) in [
            ("dune 2", "dune", 2.0),
            ("earthsea book 2", "earthsea", 2.0),
            ("stormlight archive book two", "stormlight archive", 2.0),
            ("the expanse book 3", "the expanse", 3.0),
            ("wheel of time #1", "wheel of time", 1.0),
            ("hunger games second", "hunger games", 2.0),
        ] {
            let q = parse(typed, &KnowsNothing);
            let r = q
                .reading(ReadingKind::Series)
                .unwrap_or_else(|| panic!("{typed}"));
            assert_eq!(r.series.as_deref(), Some(series), "{typed}");
            assert_eq!(r.position, Some(pos), "{typed}");
        }
        assert!(
            parse("catch 22", &KnowsNothing)
                .reading(ReadingKind::Series)
                .is_some()
        );
        assert!(
            parse("1984", &KnowsNothing)
                .reading(ReadingKind::Series)
                .is_none()
        );
        assert!(
            parse("book 2", &KnowsNothing)
                .reading(ReadingKind::Series)
                .is_none()
        );
    }

    #[test]
    fn fielded_is_as_typed() {
        let q = parse(
            "title: The Dispossessed author: Le Guin lang:fre",
            &KnowsNothing,
        );
        assert_eq!(kinds(&q), vec![ReadingKind::Fielded]);
        let r = &q.readings[0];
        assert_eq!(r.title.as_deref(), Some("the dispossessed"));
        assert_eq!(r.author.as_deref(), Some("le guin"));
        assert_eq!(q.lang.as_deref(), Some("fr"));
        assert_eq!(q.fields.title.as_deref(), Some("The Dispossessed"));
        assert_eq!(q.fields.author.as_deref(), Some("Le Guin"));

        let q = parse("isbn:978-0-441-17271-9", &KnowsNothing);
        assert_eq!(q.isbn.as_deref(), Some("9780441172719"));

        let q = parse("dune series:\"Dune Chronicles\"", &KnowsNothing);
        assert_eq!(q.readings[0].series.as_deref(), Some("dune chronicles"));
        assert_eq!(q.readings[0].title.as_deref(), Some("dune"));

        let q = parse("sapiens: a brief history", &KnowsNothing);
        assert!(has(
            &q,
            ReadingKind::WholeTitle,
            Some("sapiens a brief history"),
            None
        ));
    }

    #[test]
    fn empty_and_punctuation_only_text_reads_as_nothing() {
        assert!(parse("", &KnowsNothing).readings.is_empty());
        assert!(parse("  !!! ", &KnowsNothing).readings.is_empty());
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    fn text() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-zA-Z ]{0,30}",
            "[\\p{Latin}\\p{Cyrillic}\\p{Han}'’,.:!#–-]{0,30}",
            "[a-z]{1,8}( [a-z]{1,8}){0,5}( by| -|,)?( [a-z]{1,8}){0,3}",
        ]
    }

    proptest! {
        #[test]
        fn folding_is_idempotent(s in text()) {
            let once = fold(&s);
            prop_assert_eq!(fold(&once), once.clone());
            prop_assert!(!once.contains("  "));
            prop_assert!(once.chars().all(|c| c == ' ' || c.is_alphanumeric()), "{:?}", once);
        }

        #[test]
        fn parsing_never_loses_a_query_word(s in text()) {
            let q = parse(&s, &KnowsNothing);
            if q.isbn.is_some() {
                return Ok(());
            }
            for r in &q.readings {
                let mut said: Vec<&str> = Vec::new();
                for part in [&r.title, &r.author, &r.series].into_iter().flatten() {
                    said.extend(part.split(' '));
                }
                if r.kind == ReadingKind::WholeTitle
                    || r.kind == ReadingKind::AuthorOnly
                    || r.kind == ReadingKind::Split
                {
                    for w in &q.words {
                        prop_assert!(said.contains(&w.as_str()), "{:?} lost {:?} in {:?}", s, w, r);
                    }
                }
            }
        }

        #[test]
        fn weights_are_fractions(s in text()) {
            for r in parse(&s, &KnowsNothing).readings {
                prop_assert!(r.weight.is_finite() && (0.0..=1.0).contains(&r.weight));
            }
        }

        #[test]
        fn an_isbn_in_any_spacing_parses_the_same(
            digits in proptest::collection::vec(0u8..10, 12),
            cuts in proptest::collection::vec(any::<bool>(), 12),
            sep in prop_oneof![Just(' '), Just('-'), Just('–')],
        ) {
            let sum: u32 = digits
                .iter()
                .enumerate()
                .map(|(i, d)| u32::from(*d) * if i % 2 == 0 { 1 } else { 3 })
                .sum();
            let check = (10 - sum % 10) % 10;
            let mut plain: String = digits.iter().map(|d| char::from(b'0' + d)).collect();
            plain.push(char::from_digit(check, 10).unwrap());
            let mut spaced = String::new();
            for (i, c) in plain.chars().enumerate() {
                spaced.push(c);
                if i < 12 && cuts[i] {
                    spaced.push(sep);
                }
            }
            let a = parse(&plain, &KnowsNothing);
            let b = parse(&spaced, &KnowsNothing);
            prop_assert_eq!(a.isbn.as_deref(), Some(plain.as_str()));
            prop_assert_eq!(a.isbn, b.isbn);
        }
    }
}
