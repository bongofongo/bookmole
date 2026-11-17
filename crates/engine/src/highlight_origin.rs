//! Where a highlight came from, the unit of a stored position, and [`same_passages`], which
//! groups ids of identical words and never merges rows.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use crate::copies::string_enum;
use crate::diagnostic::Diagnostic;
use crate::error::EngineError;
use crate::storage::Highlight;

string_enum! {
    /// Which app wrote a highlight; what that app wrote belongs to it, `annotation` is bookmole's.
    HighlightOrigin {
        Koreader => "koreader",
        Preview => "preview",
        AppleBooks => "apple_books",
        Kindle => "kindle",
        KindleApp => "kindle_app",
        User => "user",
    }
}

string_enum! {
    /// The unit of a stored position. Show a page number only where the source gave one.
    PosKind {
        KoreaderXpointer => "koreader_xpointer",
        KindleLocation => "kindle_location",
        EpubCfi => "epub_cfi",
        PdfRect => "pdf_rect",
    }
}

impl HighlightOrigin {
    /// The origin implied by the legacy `highlights.source` word; unknown words mean KOReader.
    pub fn from_source(source: &str) -> HighlightOrigin {
        source.parse().unwrap_or(HighlightOrigin::Koreader)
    }
}

/// Where an importer's key (`origin_id`, else `identity`) is unique; one source always uses one
/// scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IdentityScope {
    #[default]
    Global,
    /// The key is unique only inside one file: each copy's import finds, refreshes and removes only
    /// its own rows. With `copy_id = None` this behaves as [`IdentityScope::Global`].
    Copy,
}

/// One highlight as a non-KOReader importer saw it. Fields are source-owned and refreshed toward
/// the source on re-import; `annotation` is bookmole's and an import never writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedHighlight {
    /// Never `Koreader` or `User`: KOReader has its own path and `user` rows are made in bookmole.
    pub origin: HighlightOrigin,
    /// The copy it was made in; stamped only if the stored row has none.
    pub copy_id: Option<i64>,
    pub identity_scope: IdentityScope,
    pub origin_id: Option<String>,
    /// Used when `origin_id` is `None`. Must be stable across re-reads and exclude anything the source
    /// rewrites in place (note, colour), or an edit imports as a new row.
    pub identity: String,
    pub text: String,
    pub text_unavailable: bool,
    pub origin_note: Option<String>,
    pub color: Option<String>,
    pub chapter: Option<String>,
    pub page: Option<i64>,
    pub pos_kind: Option<PosKind>,
    pub pos0: Option<String>,
    pub pos1: Option<String>,
    /// The source's creation stamp as text, stored in `ko_datetime` (shared by every importer).
    pub made_at: Option<String>,
}

impl ImportedHighlight {
    pub fn new(
        origin: HighlightOrigin,
        identity: impl Into<String>,
        text: impl Into<String>,
    ) -> ImportedHighlight {
        ImportedHighlight {
            origin,
            copy_id: None,
            identity_scope: IdentityScope::Global,
            origin_id: None,
            identity: identity.into(),
            text: text.into(),
            text_unavailable: false,
            origin_note: None,
            color: None,
            chapter: None,
            page: None,
            pos_kind: None,
            pos0: None,
            pos1: None,
            made_at: None,
        }
    }
}

/// `Updated` only when a source-owned field changed, so a second identical import is all
/// `Unchanged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportedOutcome {
    Inserted(i64),
    Updated(i64),
    Unchanged(i64),
}

impl ImportedOutcome {
    pub fn id(self) -> i64 {
        match self {
            ImportedOutcome::Inserted(id)
            | ImportedOutcome::Updated(id)
            | ImportedOutcome::Unchanged(id) => id,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HighlightImportSummary {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub unparsed: usize,
    pub diagnostics: Vec<Diagnostic>,
}

/// Comparison form for [`same_passages`]: quotes and dashes folded to ASCII, `…` to `...`,
/// lowercased, whitespace runs collapsed and trimmed. Idempotent.
pub fn normalize_passage(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' | '\u{2039}'
            | '\u{203A}' | '\u{FF07}' => folded.push('\''),
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' | '\u{00AB}'
            | '\u{00BB}' | '\u{FF02}' => folded.push('"'),
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => folded.push('-'),
            '\u{2026}' => folded.push_str("..."),
            c => folded.extend(c.to_lowercase()),
        }
    }
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Groups of two or more highlight ids with the same book and [`normalize_passage`]d words, in
/// ascending order. Rows are never merged. Empty normalised text (`text_unavailable`) is not
/// grouped.
pub fn same_passages(highlights: &[Highlight]) -> Vec<Vec<i64>> {
    let mut by_key: HashMap<(i64, String), Vec<i64>> = HashMap::new();
    for h in highlights {
        let key = normalize_passage(&h.text);
        if key.is_empty() {
            continue;
        }
        by_key.entry((h.book_id, key)).or_default().push(h.id);
    }
    let mut groups: Vec<Vec<i64>> = by_key
        .into_values()
        .filter(|g| g.len() > 1)
        .map(|mut g| {
            g.sort_unstable();
            g
        })
        .collect();
    groups.sort();
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_quotes_dashes_case_and_space() {
        assert_eq!(
            normalize_passage("  \u{201C}It\u{2019}s  \u{2014} Fine\u{2026}\u{201D}\n"),
            "\"it's - fine...\""
        );
        assert_eq!(normalize_passage("A\u{00A0}b"), "a b");
    }

    #[test]
    fn source_words_map_to_origins() {
        assert_eq!(HighlightOrigin::from_source("user"), HighlightOrigin::User);
        assert_eq!(
            HighlightOrigin::from_source("koreader"),
            HighlightOrigin::Koreader
        );
        assert_eq!(
            HighlightOrigin::from_source("whatever"),
            HighlightOrigin::Koreader
        );
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn origin_and_pos_kind_round_trip(o in 0..HighlightOrigin::ALL.len(), k in 0..PosKind::ALL.len()) {
            let (o, k) = (HighlightOrigin::ALL[o], PosKind::ALL[k]);
            prop_assert_eq!(o.as_str().parse::<HighlightOrigin>().unwrap(), o);
            prop_assert_eq!(k.to_string().parse::<PosKind>().unwrap(), k);
        }

        #[test]
        fn normalisation_is_idempotent(s in "\\PC{0,60}") {
            let once = normalize_passage(&s);
            prop_assert_eq!(normalize_passage(&once), once);
        }

        #[test]
        fn normalisation_ignores_case_and_spacing(words in proptest::collection::vec("[A-Za-z]{1,8}", 1..6)) {
            let a = words.join(" ");
            let b = words.join("  \n").to_uppercase();
            prop_assert_eq!(normalize_passage(&a), normalize_passage(&b));
        }
    }
}
