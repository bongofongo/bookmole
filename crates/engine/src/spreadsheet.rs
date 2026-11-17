//! Neutralises free-text CSV cells a spreadsheet would run as formulas (leading `=`, `+`, `-`, `@`,
//! tab, CR). Applies to the History and Goodreads exports only; in the Anki exports a prefixed `'`
//! would print on the card.

use std::borrow::Cow;

const TRIGGERS: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// Prefixes a single quote when `s` starts with a formula trigger. An already-guarded cell (quotes,
/// then a trigger) gains one more, so [`uninert_cell`] can undo exactly one.
pub fn inert_cell(s: &str) -> Cow<'_, str> {
    if s.trim_start_matches('\'').starts_with(TRIGGERS) {
        Cow::Owned(format!("'{s}"))
    } else {
        Cow::Borrowed(s)
    }
}

/// Undoes [`inert_cell`]: drops a leading `'` only when a trigger follows it.
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
