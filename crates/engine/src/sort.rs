//! The derived sort keys `books.sort_title` and `books.sort_author`, stored so `ORDER BY` can use
//! an index. `sort_author` is deliberately not on `Book`; both are written only by
//! `Storage::refresh_sort_keys`.

use crate::book::Book;
use crate::names;

/// English only, deliberately: `books.language` is mostly NULL, so a wider table would file *Das
/// Kapital* under "Kapital" on a guess. Each entry ends in a space, so "Anthem" is not "An" +
/// "them".
const ARTICLES: [&str; 3] = ["the ", "a ", "an "];

/// A title with its leading English article dropped. `None` for a blank title, never `''`: the
/// index orders `COALESCE(sort_title, title)`. Case is preserved; "The " files under "The", never
/// under nothing.
pub fn sort_title(title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    for article in ARTICLES {
        // `get` rather than an index: the title need not have a char boundary at `article.len()`.
        if title
            .get(..article.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(article))
        {
            let rest = title[article.len()..].trim_start();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    Some(title.to_string())
}

/// The Rust reading of `COALESCE(books.sort_title, books.title)`, so an in-memory reorder cannot
/// disagree with the database. Case folding is the caller's: SQLite `NOCASE` folds ASCII only, Rust
/// all Unicode.
pub fn title_key(book: &Book) -> &str {
    book.sort_title
        .as_deref()
        .or(book.title.as_deref())
        .unwrap_or("")
}

/// Separator between author-key components. It must be strictly below every byte a component can
/// hold (see [`escape`]) so `a SEP b` compares like `(a, b)`. Not `\u{0}`: C-string consumers
/// truncate at NUL.
const SEP: char = '\u{1}';

/// Removes `\u{0}`, `\u{1}` and `\u{2}` from a key component, order-preservingly and prefix-free,
/// so concatenation with [`SEP`] sorts like the tuple. `escaping_preserves_order` checks it.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{0}' => out.push_str("\u{2}\u{2}"),
            '\u{1}' => out.push_str("\u{2}\u{3}"),
            '\u{2}' => out.push_str("\u{2}\u{4}"),
            _ => out.push(c),
        }
    }
    out
}

/// [`names::sort_key`]'s tuple as one TEXT value, ordering identically under SQLite's BINARY
/// collation. `'0' SEP escape(last) SEP escape(whole)`; authorless is `'1' SEP SEP`, which sorts
/// last. Never shown.
pub fn author_key(authors: &[String]) -> String {
    let (rank, last, whole) = names::sort_key(authors);
    let rank = if rank == 0 { '0' } else { '1' };
    format!("{rank}{SEP}{}{SEP}{}", escape(&last), escape(&whole))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_article_is_dropped() {
        assert_eq!(sort_title("The Overstory").as_deref(), Some("Overstory"));
        assert_eq!(
            sort_title("A Clockwork Orange").as_deref(),
            Some("Clockwork Orange")
        );
        assert_eq!(
            sort_title("An Artist of the Floating World").as_deref(),
            Some("Artist of the Floating World")
        );
        assert_eq!(sort_title("THE ROAD").as_deref(), Some("ROAD"));
    }

    #[test]
    fn an_article_is_a_word_and_not_a_prefix() {
        assert_eq!(sort_title("Anthem").as_deref(), Some("Anthem"));
        assert_eq!(sort_title("Theft").as_deref(), Some("Theft"));
        assert_eq!(
            sort_title("Ancillary Justice").as_deref(),
            Some("Ancillary Justice")
        );
    }

    #[test]
    fn stripping_never_empties_a_title() {
        assert_eq!(sort_title("The").as_deref(), Some("The"));
        assert_eq!(sort_title("The ").as_deref(), Some("The"));
        assert_eq!(sort_title("A").as_deref(), Some("A"));
        assert_eq!(sort_title(""), None);
        assert_eq!(sort_title("   "), None);
    }

    #[test]
    fn a_non_ascii_title_is_not_sliced_through_a_character() {
        assert_eq!(sort_title("Éclair").as_deref(), Some("Éclair"));
        assert_eq!(sort_title("日").as_deref(), Some("日"));
        assert_eq!(sort_title("Ю").as_deref(), Some("Ю"));
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_authorless_book_files_after_every_author() {
        let none = author_key(&[]);
        for who in ["Zzyzx, Zoe", "Colette", "\u{10FFFF}"] {
            assert!(
                author_key(&names(&[who])) < none,
                "{who} must file before an authorless book"
            );
        }
        assert_eq!(author_key(&names(&["   "])), none);
    }

    #[test]
    fn the_key_files_under_the_last_name() {
        let mut keys = [
            author_key(&names(&["Ursula K. Le Guin"])),
            author_key(&names(&["Jorge Luis Borges"])),
            author_key(&names(&["George Eliot"])),
        ];
        keys.sort();
        assert!(keys[0].contains("borges"));
        assert!(keys[1].contains("eliot"));
        assert!(keys[2].contains("le guin"));
    }

    #[test]
    fn a_shared_surname_is_broken_by_the_whole_name() {
        let a = author_key(&names(&["Amis, Kingsley"]));
        let b = author_key(&names(&["Amis, Martin"]));
        assert!(a < b);
    }

    #[test]
    fn a_separator_inside_a_name_does_not_forge_a_component() {
        let sneaky = author_key(&names(&[&format!("Zed{SEP}Aaa")]));
        let plain = author_key(&names(&["Zed Aaa"]));
        assert_ne!(sneaky, plain);
        assert_eq!(
            sneaky.matches(SEP).count(),
            2,
            "a key has exactly two separators however many the name had: {sneaky:?}"
        );
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    fn agree(a: &[String], b: &[String]) -> bool {
        let tuple = names::sort_key(a).cmp(&names::sort_key(b));
        let stored = author_key(a).cmp(&author_key(b));
        tuple == stored
    }

    proptest! {
        #[test]
        fn the_stored_order_is_the_tuple_order(
            a in proptest::collection::vec("[\\x00-\\x08a-z, .]{0,12}", 0..3),
            b in proptest::collection::vec("[\\x00-\\x08a-z, .]{0,12}", 0..3),
        ) {
            prop_assert!(agree(&a, &b), "{a:?} vs {b:?}");
        }

        #[test]
        fn the_stored_order_is_the_tuple_order_for_real_names(
            a in proptest::collection::vec("(Le |van |)[A-Z][a-z]{1,4}(, [A-Z][a-z]{1,4}| [A-Z][a-z]{1,4})?", 0..3),
            b in proptest::collection::vec("(Le |van |)[A-Z][a-z]{1,4}(, [A-Z][a-z]{1,4}| [A-Z][a-z]{1,4})?", 0..3),
        ) {
            prop_assert!(agree(&a, &b), "{a:?} vs {b:?}");
        }

        #[test]
        fn escaping_preserves_order(a in "[\\x00-\\x05a-c]{0,8}", b in "[\\x00-\\x05a-c]{0,8}") {
            prop_assert_eq!(a.cmp(&b), super::escape(&a).cmp(&super::escape(&b)));
        }

        #[test]
        fn escaping_is_injective(a in "[\\x00-\\x05a-c]{0,8}", b in "[\\x00-\\x05a-c]{0,8}") {
            prop_assert_eq!(a == b, super::escape(&a) == super::escape(&b));
        }

        #[test]
        fn arbitrary_input_never_panics(s in ".{0,80}") {
            let _ = sort_title(&s);
            let _ = author_key(&[s]);
        }

        #[test]
        fn a_titled_book_never_files_under_nothing(s in ".{0,40}") {
            if let Some(key) = sort_title(&s) {
                prop_assert!(!key.trim().is_empty(), "{s:?} filed under {key:?}");
            } else {
                prop_assert!(s.trim().is_empty());
            }
        }
    }
}
