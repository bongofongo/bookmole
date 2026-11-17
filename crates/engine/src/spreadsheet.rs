//! Cells a spreadsheet will not execute.
//!
//! The engine writes two `.csv` files a person is likely to open in Excel,
//! Numbers or Sheets: the History chart export (`ExportHistorySeries`) and the
//! Goodreads export. Both carry **free text from a user or a provider** — a
//! book title, an author, a shelf, a review — and a spreadsheet reads a cell
//! that starts with `=`, `+`, `-` or `@` (and, in some, a tab or a carriage
//! return before one of those) as a **formula**. A title like
//! `=HYPERLINK("https://…","click")` would become a live link, and worse
//! payloads exist (CSV/formula injection, OWASP).
//!
//! [`inert_cell`] is the one rule, and every free-text cell of those two files
//! goes through it. **Not applied to the Anki exports** (`flashcards.rs`):
//! those are Anki's text-import format, read by Anki as card fields rather
//! than by a spreadsheet, and a prefixed `'` would print on the card; the
//! `#html:true` export already neutralises `"`, tabs and a leading `#` for
//! Anki's own parser. Nor to cells the engine formats itself (dates, counts,
//! ids) — a negative count does not occur, and quoting a number would turn it
//! into text.

use std::borrow::Cow;

/// The characters a spreadsheet treats as the start of a formula.
const TRIGGERS: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// `s`, prefixed with a single quote when it starts with a formula trigger —
/// the convention every major spreadsheet reads as *this cell is text* and
/// hides. Anything else comes back borrowed and unchanged.
///
/// A cell that already *looks* guarded — quotes, then a trigger — gains one
/// more quote too, so [`uninert_cell`] can always undo exactly one: without
/// it a title genuinely beginning `'=` would lose its quote on the way back.
pub fn inert_cell(s: &str) -> Cow<'_, str> {
    if s.trim_start_matches('\'').starts_with(TRIGGERS) {
        Cow::Owned(format!("'{s}"))
    } else {
        Cow::Borrowed(s)
    }
}

/// [`inert_cell`] undone: a leading `'` is dropped **only** when a trigger
/// follows it, so a reader of our own export gets back exactly the text that
/// went out. Every other cell, including one that simply starts with a quote,
/// is unchanged.
pub fn uninert_cell(s: &str) -> &str {
    match s.strip_prefix('\'') {
        Some(rest) if rest.trim_start_matches('\'').starts_with(TRIGGERS) => rest,
        _ => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_formula_title_comes_out_inert() {
        assert_eq!(
            inert_cell(r#"=HYPERLINK("https://example.invalid","click")"#),
            r#"'=HYPERLINK("https://example.invalid","click")"#
        );
        for t in ["+1", "-2+3", "@SUM(A1)", "\t=1", "\r=1"] {
            assert!(inert_cell(t).starts_with('\''), "{t:?}");
        }
        assert_eq!(inert_cell("The Long Country"), "The Long Country");
        assert_eq!(inert_cell(""), "");
        assert_eq!(
            inert_cell("A = B"),
            "A = B",
            "only the first character matters"
        );
        assert_eq!(inert_cell("'=x"), "''=x", "a lookalike gains a quote too");
        assert_eq!(uninert_cell("''=x"), "'=x");
        assert_eq!(uninert_cell("'a quote"), "'a quote");
    }

    mod props {
        use super::super::*;
        use proptest::prelude::*;

        proptest! {
            /// Nothing comes out starting with a trigger, and the text after
            /// the guard is the text that went in.
            #[test]
            fn no_cell_starts_a_formula(s in ".{0,40}") {
                let out = inert_cell(&s);
                prop_assert!(!out.starts_with(TRIGGERS));
                prop_assert!(out == s || (out.starts_with('\'') && out[1..] == s));
                prop_assert_eq!(uninert_cell(&out), s.as_str(), "the guard round-trips");
            }
        }
    }
}
