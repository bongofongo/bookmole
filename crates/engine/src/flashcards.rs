use crate::storage::FlashcardRow;

pub fn single_word(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.split_whitespace().count() != 1 {
        return None;
    }
    let cleaned = trimmed.trim_matches(|c: char| !c.is_alphanumeric());
    if cleaned.is_empty() {
        return None;
    }
    Some(cleaned.to_string())
}

fn escape_tsv(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

pub fn export_tsv(cards: &[FlashcardRow]) -> String {
    let mut out = String::from("#separator:tab\n#html:false\n");
    for c in cards {
        out.push_str(&format!(
            "{}\t{}\t{}\n",
            escape_tsv(&c.word),
            escape_tsv(c.context.as_deref().unwrap_or("")),
            escape_tsv(&c.book_title),
        ));
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnkiExport {
    pub text: String,
    pub count: usize,
    pub filename: String,
}

/// `path` is `None` when no card went out and no file was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedAnkiExport {
    pub path: Option<std::path::PathBuf>,
    pub count: usize,
}

/// `::` is Anki's sub-deck separator, so each book becomes a child of this deck.
pub const ANKI_DECK_ROOT: &str = "bookmole";

pub const ANKI_TAG: &str = "bookmole";

#[derive(Debug, Clone)]
pub struct AnkiCard<'a> {
    pub card: &'a FlashcardRow,
    pub authors: &'a [String],
}

/// Stable GUID `bookmole-<card id>-<first 8 hex of sha256(word)>` so Anki updates rather than
/// duplicates.
/// The id survives `merge_books` (which rewrites `book_id`); the word hash guards against SQLite
/// reusing a deleted id.
pub fn anki_guid(card: &FlashcardRow) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(card.word.as_bytes());
    let short: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("bookmole-{}-{short}", card.id)
}

/// HTML-escapes a field for `#html:true`: `"` becomes `&quot;`, tab a space, newline `<br>`, and a
/// leading `#`
/// becomes `&#35;`, since a row starting with `#` is an Anki comment line.
pub fn anki_html(s: &str) -> String {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(s.len() + 8);
    for (i, ch) in s.chars().enumerate() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\n' => out.push_str("<br>"),
            '\t' => out.push(' '),
            '#' if i == 0 => out.push_str("&#35;"),
            c => out.push(c),
        }
    }
    out
}

/// `bookmole::<title>`. Deck names are plain text: runs of colons collapse to one (`::` would make
/// a sub-deck),
/// control characters become spaces, `"` becomes `'`, and an empty title falls back to `book <id>`.
pub fn anki_deck(card: &FlashcardRow) -> String {
    let mut clean = String::with_capacity(card.book_title.len());
    let mut last_colon = false;
    for ch in card.book_title.chars() {
        let ch = if ch.is_control() {
            ' '
        } else if ch == '"' {
            '\''
        } else {
            ch
        };
        if ch == ':' {
            if !last_colon {
                clean.push(':');
            }
            last_colon = true;
            continue;
        }
        last_colon = false;
        clean.push(ch);
    }
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean = clean.trim_matches(|c: char| c == ':' || c.is_whitespace());
    if clean.is_empty() {
        format!("{ANKI_DECK_ROOT}::book {}", card.book_id)
    } else {
        format!("{ANKI_DECK_ROOT}::{clean}")
    }
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

pub fn anki_tags(card: &FlashcardRow) -> String {
    let book = slug(&card.book_title);
    let book = if book.is_empty() {
        format!("book-{}", card.book_id)
    } else {
        book
    };
    format!("{ANKI_TAG} {ANKI_TAG}::{book}")
}

fn anki_back(card: &FlashcardRow, authors: &[String]) -> String {
    let mut cite = anki_html(card.book_title.trim());
    let authors: Vec<&str> = authors
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .collect();
    if !authors.is_empty() {
        if !cite.is_empty() {
            cite.push_str(" — ");
        }
        cite.push_str(&anki_html(&authors.join(", ")));
    }
    let context = card
        .context
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    match (context, cite.is_empty()) {
        (Some(ctx), false) => format!("{}<br><br><small>{cite}</small>", anki_html(ctx)),
        (Some(ctx), true) => anki_html(ctx),
        (None, false) => format!("<small>{cite}</small>"),
        (None, true) => String::new(),
    }
}

/// Anki's file-header directives in the manual's order; columns are 1-indexed: Front, Back, Deck,
/// Tags, GUID.
pub const ANKI_HEADER: &str = "#separator:tab\n\
#html:true\n\
#notetype:Basic\n\
#deck column:3\n\
#tags column:4\n\
#guid column:5\n";

pub fn export_anki(cards: &[AnkiCard<'_>]) -> String {
    let mut out = String::from(ANKI_HEADER);
    for AnkiCard { card, authors } in cards {
        out.push_str(&anki_html(card.word.trim()));
        out.push('\t');
        out.push_str(&anki_back(card, authors));
        out.push('\t');
        out.push_str(&anki_deck(card));
        out.push('\t');
        out.push_str(&anki_tags(card));
        out.push('\t');
        out.push_str(&anki_guid(card));
        out.push('\n');
    }
    out
}

/// `bookmole-anki[-<book>]-<stamp>.txt`. The stamp is the caller's to keep this pure; each export
/// gets a new name
/// since exporting marks cards and overwriting an unimported file would lose them.
pub fn anki_filename(book_title: Option<&str>, stamp: &str) -> String {
    match book_title.map(slug).filter(|s| !s.is_empty()) {
        Some(book) => {
            // Bounded so a very long title still makes a usable name.
            let book: String = book.chars().take(48).collect();
            let book = book.trim_end_matches('-');
            format!("bookmole-anki-{book}-{stamp}.txt")
        }
        None => format!("bookmole-anki-{stamp}.txt"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_word_detection() {
        assert_eq!(single_word("middlemarch"), Some("middlemarch".to_string()));
        assert_eq!(single_word("  «mot»  "), Some("mot".to_string()));
        assert_eq!(single_word("\"verdict,\""), Some("verdict".to_string()));
        assert_eq!(single_word("well-known"), Some("well-known".to_string()));
        assert_eq!(single_word("two words"), None);
        assert_eq!(single_word(""), None);
        assert_eq!(single_word("..."), None);
    }

    #[test]
    fn tsv_escapes_separators() {
        let cards = vec![FlashcardRow {
            id: 1,
            book_id: 1,
            highlight_id: None,
            word: "mot".into(),
            context: Some("line one\nline\ttwo".into()),
            book_title: "A Book".into(),
            exported: false,
        }];
        let tsv = export_tsv(&cards);
        let lines: Vec<&str> = tsv.lines().collect();
        assert_eq!(lines[0], "#separator:tab");
        assert_eq!(lines[1], "#html:false");
        assert_eq!(lines[2], "mot\tline one line two\tA Book");
    }

    fn card(id: i64, word: &str, context: Option<&str>, title: &str) -> FlashcardRow {
        FlashcardRow {
            id,
            book_id: 7,
            highlight_id: None,
            word: word.into(),
            context: context.map(str::to_string),
            book_title: title.into(),
            exported: false,
        }
    }

    fn one(c: &FlashcardRow, authors: &[String]) -> Vec<String> {
        let text = export_anki(&[AnkiCard { card: c, authors }]);
        let row = text.lines().nth(6).expect("a row after six headers");
        row.split('\t').map(str::to_string).collect()
    }

    #[test]
    fn anki_headers_are_the_documented_six_in_order() {
        let text = export_anki(&[]);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            [
                "#separator:tab",
                "#html:true",
                "#notetype:Basic",
                "#deck column:3",
                "#tags column:4",
                "#guid column:5",
            ]
        );
    }

    #[test]
    fn anki_row_is_front_back_deck_tags_guid() {
        let c = card(
            12,
            "middlemarch",
            Some("a market town in the Midlands"),
            "Middlemarch",
        );
        let row = one(&c, &["George Eliot".into()]);
        assert_eq!(row.len(), 5);
        assert_eq!(row[0], "middlemarch");
        assert_eq!(
            row[1],
            "a market town in the Midlands<br><br><small>Middlemarch — George Eliot</small>"
        );
        assert_eq!(row[2], "bookmole::Middlemarch");
        assert_eq!(row[3], "bookmole bookmole::middlemarch");
        assert!(row[4].starts_with("bookmole-12-"), "{}", row[4]);
    }

    #[test]
    fn anki_escapes_tab_newline_angle_and_ampersand() {
        let c = card(
            1,
            "<b>&mot",
            Some("line one\r\nline\ttwo <i>x</i> & \"y\""),
            "A & B",
        );
        let row = one(&c, &[]);
        assert_eq!(row.len(), 5, "a tab leaked into a field: {row:?}");
        assert_eq!(row[0], "&lt;b&gt;&amp;mot");
        assert_eq!(
            row[1],
            "line one<br>line two &lt;i&gt;x&lt;/i&gt; &amp; &quot;y&quot;<br><br><small>A &amp; B</small>"
        );
        assert_eq!(row[2], "bookmole::A & B");
    }

    #[test]
    fn anki_deck_neutralises_double_colons_tabs_and_quotes() {
        let c = card(1, "w", None, "Part::One:::Two\tThree \"Q\"");
        assert_eq!(anki_deck(&c), "bookmole::Part:One:Two Three 'Q'");
        assert_eq!(
            anki_deck(&card(1, "w", None, "Dune: Messiah")),
            "bookmole::Dune: Messiah"
        );
        assert_eq!(anki_deck(&card(1, "w", None, " :: ")), "bookmole::book 7");
    }

    #[test]
    fn anki_tags_never_hold_a_space() {
        let c = card(1, "w", None, "L'Étranger: Roman  \t(1942)");
        assert_eq!(anki_tags(&c), "bookmole bookmole::l-étranger-roman-1942");
        assert_eq!(
            anki_tags(&card(1, "w", None, "!!!")),
            "bookmole bookmole::book-7"
        );
    }

    #[test]
    fn anki_card_with_no_context_is_the_citation_alone() {
        let row = one(
            &card(3, "ardent", None, "Middlemarch"),
            &["George Eliot".into()],
        );
        assert_eq!(row[1], "<small>Middlemarch — George Eliot</small>");
        let row = one(&card(3, "ardent", Some("   "), "Middlemarch"), &[]);
        assert_eq!(row[1], "<small>Middlemarch</small>");
    }

    #[test]
    fn anki_card_whose_highlight_is_gone_keeps_its_context() {
        let mut c = card(4, "ghost", Some("still here"), "B");
        c.highlight_id = Some(9_999);
        let row = one(&c, &[]);
        assert_eq!(row[1], "still here<br><br><small>B</small>");
    }

    #[test]
    fn anki_leading_hash_is_not_a_comment_line() {
        let row = one(&card(5, "#hashtag", Some("#1"), "B"), &[]);
        assert_eq!(row[0], "&#35;hashtag");
        assert!(row[1].starts_with("&#35;1"));
    }

    #[test]
    fn anki_guid_is_stable_and_word_sensitive() {
        let a = card(9, "mot", Some("one"), "Book");
        let mut moved = a.clone();
        moved.book_id = 99;
        moved.book_title = "Other edition".into();
        moved.context = Some("edited".into());
        assert_eq!(anki_guid(&a), anki_guid(&moved));
        assert_ne!(anki_guid(&a), anki_guid(&card(9, "autre", None, "Book")));
        let g = anki_guid(&a);
        let hash = g.strip_prefix("bookmole-9-").expect("prefix");
        assert_eq!(hash.len(), 8);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{g}");
    }

    #[test]
    fn anki_filename_is_bounded_and_scoped() {
        assert_eq!(
            anki_filename(None, "20261018-130500"),
            "bookmole-anki-20261018-130500.txt"
        );
        assert_eq!(
            anki_filename(Some("Dune: Messiah"), "s"),
            "bookmole-anki-dune-messiah-s.txt"
        );
        let long = "x ".repeat(200);
        assert!(anki_filename(Some(&long), "s").len() < 80);
        assert_eq!(anki_filename(Some("???"), "s"), "bookmole-anki-s.txt");
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    fn row(word: &str, context: Option<&str>, title: &str) -> FlashcardRow {
        FlashcardRow {
            id: 1,
            book_id: 1,
            highlight_id: None,
            word: word.to_string(),
            context: context.map(str::to_string),
            book_title: title.to_string(),
            exported: false,
        }
    }

    proptest! {
        #[test]
        fn every_exported_row_has_exactly_three_fields(
            cards in proptest::collection::vec(
                (".{0,30}", proptest::option::of(".{0,30}"), ".{0,30}"),
                0..6,
            ),
        ) {
            let rows: Vec<FlashcardRow> = cards
                .iter()
                .map(|(w, c, t)| row(w, c.as_deref(), t))
                .collect();
            let tsv = export_tsv(&rows);

            let lines: Vec<&str> = tsv.lines().collect();
            prop_assert_eq!(lines.len(), 2 + rows.len(), "row count changed: {:?}", tsv);
            prop_assert!(lines[0].starts_with('#') && lines[1].starts_with('#'));

            for line in lines.iter().skip(2) {
                prop_assert_eq!(
                    line.split('\t').count(),
                    3,
                    "not a 3-field row: {:?}", line
                );
            }
        }

        #[test]
        fn every_anki_row_has_exactly_five_fields(
            cards in proptest::collection::vec(
                (".{0,30}", proptest::option::of(".{0,30}"), ".{0,30}", ".{0,12}"),
                0..6,
            ),
        ) {
            let rows: Vec<FlashcardRow> = cards
                .iter()
                .map(|(w, c, t, _)| row(w, c.as_deref(), t))
                .collect();
            let authors: Vec<Vec<String>> = cards.iter().map(|(.., a)| vec![a.clone()]).collect();
            let notes: Vec<AnkiCard<'_>> = rows
                .iter()
                .zip(&authors)
                .map(|(card, a)| AnkiCard { card, authors: a })
                .collect();
            let text = export_anki(&notes);
            let lines: Vec<&str> = text.lines().collect();
            prop_assert_eq!(lines.len(), 6 + rows.len(), "row count changed: {:?}", text);
            for line in lines.iter().skip(6) {
                let fields: Vec<&str> = line.split('\t').collect();
                prop_assert_eq!(fields.len(), 5, "not a 5-field row: {:?}", line);
                prop_assert!(!line.starts_with('#'), "a comment line: {:?}", line);
                for f in fields {
                    prop_assert!(!f.starts_with('"'), "opens a quoted field: {:?}", f);
                }
            }
        }

        #[test]
        fn a_single_word_is_really_a_single_clean_word(s in ".{0,40}") {
            if let Some(w) = single_word(&s) {
                prop_assert!(!w.is_empty());
                prop_assert!(
                    !w.chars().any(char::is_whitespace),
                    "whitespace in {:?} from {:?}", w, s
                );
                prop_assert!(w.chars().next().unwrap().is_alphanumeric(), "{:?}", w);
                prop_assert!(w.chars().last().unwrap().is_alphanumeric(), "{:?}", w);
                prop_assert!(s.contains(&w), "{:?} is not a substring of {:?}", w, s);
            }
        }

        #[test]
        fn multi_word_text_is_never_a_flashcard(a in "[a-z]{1,8}", b in "[a-z]{1,8}") {
            prop_assert_eq!(single_word(&format!("{a} {b}")), None);
        }

        #[test]
        fn single_word_never_panics(s in ".{0,100}") {
            let _ = single_word(&s);
        }
    }
}
