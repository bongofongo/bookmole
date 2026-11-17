//! Where a highlight came from (migration `0030`).
//!
//! [`HighlightOrigin`] is the typed `highlights.origin`; [`PosKind`] names the
//! unit a stored position is in (`highlights.pos_kind`) — positions are kept in
//! the source's own unit and never converted. [`same_passages`] is the derived
//! fact that two apps highlighted the same words: it groups ids and **never
//! merges rows**.
//!
//! Highlight text and notes are the user's private reading: nothing here logs
//! them.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use crate::copies::string_enum;
use crate::diagnostic::Diagnostic;
use crate::error::EngineError;
use crate::storage::Highlight;

string_enum! {
    /// Which app wrote a highlight. Whatever that app wrote (text, its note, its
    /// colour) belongs to it; `annotation` is always bookmole's.
    HighlightOrigin {
        Koreader => "koreader",
        Preview => "preview",
        AppleBooks => "apple_books",
        /// The e-reader's `My Clippings.txt`.
        Kindle => "kindle",
        /// The Kindle app's Export Notebook file.
        KindleApp => "kindle_app",
        /// Made in bookmole.
        User => "user",
    }
}

string_enum! {
    /// The unit of a stored position. Show a page number only where the source
    /// gave one: a converted position is a guess shown as a fact.
    PosKind {
        KoreaderXpointer => "koreader_xpointer",
        KindleLocation => "kindle_location",
        EpubCfi => "epub_cfi",
        PdfRect => "pdf_rect",
    }
}

impl HighlightOrigin {
    /// The origin implied by the legacy `highlights.source` word. Only `user`
    /// is not KOReader today, because KOReader was the only importer before
    /// `0030`.
    pub fn from_source(source: &str) -> HighlightOrigin {
        source.parse().unwrap_or(HighlightOrigin::Koreader)
    }
}

/// Where an importer's key (`origin_id`, else `identity`) is unique, which
/// decides whether the copy takes part in finding the stored row. See
/// `storage/imported_highlights.rs`, "Identity", for what each one does.
///
/// It is a property of the **source**, so one origin always uses one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IdentityScope {
    /// The key names one highlight wherever it is seen: Apple Books' annotation
    /// uuid, a Kindle clipping's title + location + text. The copy plays no
    /// part in finding the row, so an import with `copy_id = None` and one with
    /// `Some` (or another copy) refresh the same row.
    #[default]
    Global,
    /// The key is only unique **inside one file**: a PDF annotation's `/NM`, or
    /// its page and rect. Two copies of a book holding the same key are two
    /// highlights, and each copy's import finds, refreshes and removes only its
    /// own rows (and claims a row stored with no copy). With `copy_id = None`
    /// there is no copy to scope by and this behaves as [`IdentityScope::Global`].
    Copy,
}

/// One highlight as a non-KOReader importer saw it. Everything here is what the
/// **source app** said, so on a re-import it is refreshed toward the source;
/// `annotation` (and anything else bookmole owns) is not in this struct
/// because an import never writes it. Highlight text and notes are the user's
/// private reading: nothing that holds an `ImportedHighlight` may log them.
///
/// [`ImportedHighlight::new`] fills every optional field with "the source did
/// not say" and the scope with [`IdentityScope::Global`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedHighlight {
    /// Which app wrote it. Never [`HighlightOrigin::Koreader`] or
    /// [`HighlightOrigin::User`]: KOReader has its own path and a `user` row is
    /// made in bookmole, not imported.
    pub origin: HighlightOrigin,
    /// The copy it was made in; stamped only if the stored row has none.
    pub copy_id: Option<i64>,
    /// Whether `origin_id` / `identity` are unique per source or only per copy.
    /// [`IdentityScope::Global`] unless the importer's key is a per-file fact.
    pub identity_scope: IdentityScope,
    /// The source's own stable id, when it has one (Apple Books' annotation
    /// uuid, a PDF annotation's `/NM`). Takes precedence over `identity`.
    pub origin_id: Option<String>,
    /// The importer's identity key, used when `origin_id` is `None`. It must be
    /// stable across re-reads and must not contain anything the source app
    /// rewrites in place (the note, the colour), or an edit imports as a new row.
    pub identity: String,
    pub text: String,
    /// The highlight is real but its words could not be extracted.
    pub text_unavailable: bool,
    pub origin_note: Option<String>,
    pub color: Option<String>,
    pub chapter: Option<String>,
    /// Only when the source gave one.
    pub page: Option<i64>,
    pub pos_kind: Option<PosKind>,
    pub pos0: Option<String>,
    pub pos1: Option<String>,
    /// The source's creation stamp, as text (stored in `ko_datetime`, the
    /// column every importer's "when" lives in; it is not a KOReader-only fact).
    pub made_at: Option<String>,
}

impl ImportedHighlight {
    /// A highlight of `origin` keyed by `identity`, with `text`, and nothing
    /// else said: no copy, no `origin_id`, [`IdentityScope::Global`], no note,
    /// colour, chapter, page, position or stamp, `text_unavailable = false`.
    /// Set the rest as fields.
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

/// What [`crate::Storage::upsert_imported_highlight`] did to a row. `Updated`
/// only when a source-owned field actually changed, so a second identical import
/// is all `Unchanged`.
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

/// The result of one importer pass over one source. Counts are of highlights.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HighlightImportSummary {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    /// Rows deleted because the source no longer has them (only for sources
    /// whose own behaviour is to forget; see the importer).
    pub removed: usize,
    /// Entries in the source that were seen but could not be made into a
    /// highlight at all.
    pub unparsed: usize,
    pub diagnostics: Vec<Diagnostic>,
}

/// The comparison form of a passage's words, for [`same_passages`].
///
/// 1. curly/low/angle single quotes and primes fold to `'`, double ones to `"`;
/// 2. every dash-like mark (hyphen, non-breaking hyphen, figure, en, em, bar,
///    minus) folds to `-`;
/// 3. `…` becomes `...`;
/// 4. lowercase;
/// 5. any run of whitespace (including NBSP and newlines) collapses to one
///    space, ends trimmed.
///
/// Idempotent: the output contains none of the characters steps 1-3 rewrite,
/// lowercasing is a fixed point on its own output, and a collapsed string
/// collapses to itself.
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

/// Groups of highlight ids that are the same passage: same book and the same
/// [`normalize_passage`]d words, with **two or more** members. A highlight
/// nobody else shares appears in no group. Groups and the ids inside them are in
/// ascending order. Rows are never merged — each keeps its own origin, note and
/// colour; a frontend may show a group as one passage with its sources.
///
/// Passages with empty normalised text (a `text_unavailable` highlight) are not
/// grouped: nothing says they are the same words.
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
