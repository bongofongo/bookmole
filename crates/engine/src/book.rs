use time::OffsetDateTime;

/// Internal `id` is canonical; ISBNs are unique lookup keys (a book may carry either, both or
/// neither).
#[derive(Debug, Clone, Default)]
pub struct Book {
    pub id: Option<i64>,
    pub title: Option<String>,
    pub sort_title: Option<String>,
    pub authors: Vec<String>,
    pub translators: Vec<String>,
    pub publisher: Option<String>,
    pub publish_year: Option<i64>,
    /// Never derived from `publish_year`, which is often an edition's year; `None` means no source
    /// has said.
    pub first_published: Option<i64>,
    pub language: Option<String>,
    pub isbn_10: Option<String>,
    pub isbn_13: Option<String>,
    pub openlibrary_key: Option<String>,
    pub googlebooks_id: Option<String>,
    pub cover_url: Option<String>,
    pub cover_path: Option<String>,
    pub page_count: Option<i64>,
    pub description: Option<String>,
    pub first_sentence: Option<String>,
    /// What a provider says the book is about, raw as spelled; empty means the record does not say.
    /// Not `book_tags`; a merge takes one provider's set whole, never a union.
    pub subjects: Vec<String>,
    /// A pair with `series_index`: user-ownership of either half protects both in merges.
    pub series: Option<String>,
    /// Fractional on purpose: novellas are numbered 0.5 and 1.5.
    pub series_index: Option<f64>,
    /// Measured by `Storage::set_cover` beside `cover_path`; merges ignore them. `None` means not
    /// measured yet, never "no cover".
    pub cover_width: Option<i64>,
    pub cover_height: Option<i64>,
    /// Median colour of a 2px frame around the cover, packed `0xRRGGBB`. Unclamped: any luma policy
    /// is the renderer's.
    pub cover_accent: Option<i64>,
    /// Downscaled shelf tier; `None` when the cover is its own thumbnail (see `shelf_cover_path`).
    pub cover_thumb_path: Option<String>,
    /// Reading state: a read-only projection of the current reading (the open one, else the most
    /// recent). `upsert_book` ignores these; `update_progress` writes them.
    pub current_page: Option<i64>,
    pub finished: bool,
    /// The current reading's `status` (`reading`, `finished`, `abandoned`), or `None` with no reading.
    /// A `String` because an importer can write a status this build does not know.
    pub reading_status: Option<String>,
    /// The device's own fraction of the book, `0.0..=1.0`; it can disagree with `current_page`, and
    /// `Progress` decides which wins.
    pub ko_percent: Option<f64>,
    pub date_started: Option<i64>,
    pub date_finished: Option<i64>,
    pub created_at: Option<OffsetDateTime>,
    pub last_modified: Option<OffsetDateTime>,
}

impl Book {
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or("(untitled)")
    }

    pub fn display_authors(&self) -> String {
        if self.authors.is_empty() {
            "(unknown)".to_string()
        } else {
            self.authors.join(", ")
        }
    }

    pub fn any_isbn(&self) -> Option<&str> {
        self.isbn_13.as_deref().or(self.isbn_10.as_deref())
    }

    /// Width over height; `None` when unmeasured (a zero dimension is a broken measurement, not a
    /// shape).
    pub fn cover_aspect(&self) -> Option<f64> {
        match (self.cover_width, self.cover_height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Some(w as f64 / h as f64),
            _ => None,
        }
    }

    pub fn cover_accent_rgb(&self) -> Option<[u8; 3]> {
        self.cover_accent.map(crate::images::accent_channels)
    }

    pub fn shelf_cover_path(&self) -> Option<&str> {
        self.cover_thumb_path
            .as_deref()
            .or(self.cover_path.as_deref())
    }

    /// `Dune #2`, or bare `Dune` with no index. Shares `series_index_text` so spellings cannot differ.
    pub fn series_label(&self) -> Option<String> {
        let name = self.series.as_deref()?;
        Some(match self.series_index {
            Some(n) => format!("{name} #{}", series_index_text(n)),
            None => name.to_string(),
        })
    }

    pub fn reading_state(&self) -> Option<ReadingState> {
        self.reading_status.as_deref().map(ReadingState::from)
    }

    /// `Jorge Luis Borges`, never `Borges, Jorge Luis`; a list because joining names is a frontend's
    /// wording.
    pub fn authors_in_display_order(&self) -> Vec<String> {
        self.authors
            .iter()
            .map(|a| crate::names::display_order(a))
            .collect()
    }

    pub fn canonical_isbn13(&self) -> Option<String> {
        if let Some(i13) = &self.isbn_13 {
            return Some(i13.clone());
        }
        self.isbn_10.as_deref().and_then(isbn10_to_13)
    }
}

/// A reading's `status`, typed. No `NeverOpened` variant on purpose: a book with no reading is
/// `None`, so nothing counts unread books. `Other` keeps an unknown status whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadingState {
    Reading,
    Finished,
    /// Put down; `abandon_reading` leaves the reading open, so `finished: false` cannot tell this
    /// from an active read.
    Abandoned,
    Other(String),
}

impl ReadingState {
    pub fn is_unknown(&self) -> bool {
        matches!(self, ReadingState::Other(_))
    }
}

impl From<&str> for ReadingState {
    fn from(s: &str) -> Self {
        match s {
            "reading" => ReadingState::Reading,
            "finished" => ReadingState::Finished,
            "abandoned" => ReadingState::Abandoned,
            other => ReadingState::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for ReadingState {
    /// The stored vocabulary, not a label; round-trips through `From<&str>` so unknown statuses
    /// survive.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadingState::Reading => write!(f, "reading"),
            ReadingState::Finished => write!(f, "finished"),
            ReadingState::Abandoned => write!(f, "abandoned"),
            ReadingState::Other(s) => write!(f, "{s}"),
        }
    }
}

/// A series index as a person writes it: `2`, never `2.0`; `0.5`, never `0.5000`.
pub fn series_index_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Strips hyphens, spaces and `urn:isbn:`, uppercases X, and validates the checksum.
pub fn normalize_isbn(raw: &str) -> Option<String> {
    let stripped = raw.trim().rsplit(':').next().unwrap_or(raw);
    let cleaned: String = stripped
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    match cleaned.len() {
        10 if is_valid_isbn10(&cleaned) => Some(cleaned),
        13 if is_valid_isbn13(&cleaned) => Some(cleaned),
        _ => None,
    }
}

fn is_valid_isbn10(s: &str) -> bool {
    let mut sum: u32 = 0;
    for (i, c) in s.chars().enumerate() {
        let v = match c {
            '0'..='9' => c as u32 - '0' as u32,
            'X' if i == 9 => 10,
            _ => return false,
        };
        sum += (10 - i as u32) * v;
    }
    sum.is_multiple_of(11)
}

fn is_valid_isbn13(s: &str) -> bool {
    let mut sum: u32 = 0;
    for (i, c) in s.chars().enumerate() {
        let Some(v) = c.to_digit(10) else {
            return false;
        };
        sum += if i % 2 == 0 { v } else { 3 * v };
    }
    sum.is_multiple_of(10)
}

pub fn isbn10_to_13(isbn10: &str) -> Option<String> {
    if isbn10.len() != 10 || !is_valid_isbn10(isbn10) {
        return None;
    }
    let body = format!("978{}", &isbn10[..9]);
    let mut sum: u32 = 0;
    for (i, c) in body.chars().enumerate() {
        let v = c.to_digit(10)?;
        sum += if i % 2 == 0 { v } else { 3 * v };
    }
    let check = (10 - sum % 10) % 10;
    Some(format!("{body}{check}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_state_round_trips_through_its_stored_word() {
        for w in ["reading", "finished", "abandoned", "tsundoku"] {
            assert_eq!(ReadingState::from(w).to_string(), w);
        }
        assert!(ReadingState::from("tsundoku").is_unknown());
        assert!(!ReadingState::from("abandoned").is_unknown());
    }

    #[test]
    fn no_reading_is_absence_rather_than_a_state() {
        let b = Book::default();
        assert_eq!(b.reading_state(), None);
    }

    #[test]
    fn authors_are_handed_over_one_at_a_time_in_reading_order() {
        let b = Book {
            authors: vec!["Borges, Jorge Luis".into(), "Colette".into()],
            ..Book::default()
        };
        assert_eq!(
            b.authors_in_display_order(),
            ["Jorge Luis Borges", "Colette"]
        );
    }

    #[test]
    fn normalizes_hyphens_and_prefixes() {
        assert_eq!(
            normalize_isbn("urn:isbn:978-1-900000-17-8"),
            Some("9781900000178".to_string())
        );
        assert_eq!(
            normalize_isbn(" 0-306-40615-2 "),
            Some("0306406152".to_string())
        );
    }

    #[test]
    fn keeps_leading_zero_and_x_check_digit() {
        assert_eq!(
            normalize_isbn("0-19-853453-1"),
            Some("0198534531".to_string())
        );
        assert_eq!(normalize_isbn("080442957x"), Some("080442957X".to_string()));
    }

    #[test]
    fn rejects_bad_checksums_and_shapes() {
        assert_eq!(normalize_isbn("9781900000179"), None);
        assert_eq!(normalize_isbn("0306406153"), None);
        assert_eq!(normalize_isbn("12345"), None);
        assert_eq!(normalize_isbn("not-an-isbn"), None);
        assert_eq!(normalize_isbn("123456789X123"), None); // X only legal in isbn10
    }

    #[test]
    fn converts_10_to_13() {
        assert_eq!(
            isbn10_to_13("0306406152"),
            Some("9780306406157".to_string())
        );
        assert_eq!(
            isbn10_to_13("155404295X"),
            Some("9781554042951".to_string())
        );
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    fn valid_isbn10() -> impl Strategy<Value = String> {
        proptest::collection::vec(0u32..10, 9).prop_map(|digits| {
            let sum: u32 = digits
                .iter()
                .enumerate()
                .map(|(i, d)| (10 - i as u32) * d)
                .sum();
            let check = (11 - (sum % 11)) % 11;
            let mut s: String = digits
                .iter()
                .map(|d| char::from_digit(*d, 10).unwrap())
                .collect();
            s.push(if check == 10 {
                'X'
            } else {
                char::from_digit(check, 10).unwrap()
            });
            s
        })
    }

    fn isbn13_check(body: &[u32]) -> u32 {
        let sum: u32 = body
            .iter()
            .enumerate()
            .map(|(i, d)| if i % 2 == 0 { *d } else { 3 * d })
            .sum();
        (10 - sum % 10) % 10
    }

    fn isbn13_with_a_five_gap_pair() -> impl Strategy<Value = (String, usize)> {
        (
            proptest::collection::vec(0u32..10, 12),
            0usize..11,
            0u32..5,
            any::<bool>(),
        )
            .prop_map(|(mut body, pos, low, flip)| {
                let (a, b) = if flip { (low + 5, low) } else { (low, low + 5) };
                body[pos] = a;
                body[pos + 1] = b;
                let check = isbn13_check(&body);
                let mut s: String = body
                    .iter()
                    .map(|d| char::from_digit(*d, 10).unwrap())
                    .collect();
                s.push(char::from_digit(check, 10).unwrap());
                (s, pos)
            })
    }

    fn valid_isbn13() -> impl Strategy<Value = String> {
        valid_isbn10().prop_map(|t| isbn10_to_13(&t).expect("generator produced a valid isbn10"))
    }

    proptest! {
        #[test]
        fn isbn10_converts_to_a_genuinely_valid_isbn13(t in valid_isbn10()) {
            let thirteen = isbn10_to_13(&t).expect("valid 10 must convert");
            prop_assert_eq!(thirteen.len(), 13);
            prop_assert!(thirteen.starts_with("978"));
            prop_assert!(is_valid_isbn13(&thirteen), "bad checksum: {}", thirteen);
            let round = normalize_isbn(&thirteen);
            prop_assert_eq!(round.as_deref(), Some(thirteen.as_str()));
        }

        #[test]
        fn normalize_is_idempotent(t in valid_isbn10()) {
            let once = normalize_isbn(&t).unwrap();
            let twice = normalize_isbn(&once);
            prop_assert_eq!(twice.as_deref(), Some(once.as_str()));
        }

        #[test]
        fn formatting_never_changes_the_result(
            t in valid_isbn10(),
            cuts in proptest::collection::vec(0usize..10, 0..4),
        ) {
            let mut decorated = t.clone();
            // Insert at descending positions so earlier inserts don't shift later ones.
            let mut cuts = cuts;
            cuts.sort_unstable();
            cuts.dedup();
            for at in cuts.into_iter().rev() {
                decorated.insert(at, '-');
            }
            let variants = [
                decorated.clone(),
                format!("urn:isbn:{decorated}"),
                format!("  {decorated}  "),
                decorated.replace('-', " "),
                decorated.to_lowercase(),
            ];
            for v in variants {
                let got = normalize_isbn(&v);
                prop_assert_eq!(
                    got.as_deref(),
                    Some(t.as_str()),
                    "decoration changed the parse: {:?}", v
                );
            }
        }

        #[test]
        fn a_single_wrong_digit_invalidates_an_isbn13(
            t in valid_isbn13(),
            pos in 0usize..13,
            delta in 1u32..10,
        ) {
            let mut bytes: Vec<char> = t.chars().collect();
            let orig = bytes[pos].to_digit(10).unwrap();
            let changed = (orig + delta) % 10;
            prop_assume!(changed != orig);
            bytes[pos] = char::from_digit(changed, 10).unwrap();
            let mutated: String = bytes.into_iter().collect();
            prop_assert!(
                !is_valid_isbn13(&mutated),
                "single-digit error slipped through: {} -> {}", t, mutated
            );
        }

        /// ISBN-10 is mod 11 with distinct weights: swapping adjacent unequal digits always breaks it.
        #[test]
        fn adjacent_transposition_always_breaks_an_isbn10(
            t in valid_isbn10(),
            pos in 0usize..9,
        ) {
            let mut c: Vec<char> = t.chars().collect();
            prop_assume!(c[pos] != c[pos + 1]);
            prop_assume!(c[pos].is_ascii_digit() && c[pos + 1].is_ascii_digit());
            c.swap(pos, pos + 1);
            let swapped: String = c.into_iter().collect();
            prop_assert!(
                !is_valid_isbn10(&swapped),
                "mod-11 must catch every adjacent transposition: {} -> {}", t, swapped
            );
        }

        /// ISBN-13 cannot see a swap of adjacent digits differing by 5 (the checksum moves by 10). The
        /// pair is constructed, not filtered for: assuming it would hit the reject cap.
        #[test]
        fn isbn13_provably_misses_transpositions_differing_by_five(
            (t, pos) in isbn13_with_a_five_gap_pair(),
        ) {
            let mut c: Vec<char> = t.chars().collect();
            let a = c[pos].to_digit(10).unwrap();
            let b = c[pos + 1].to_digit(10).unwrap();
            prop_assert_eq!(a.abs_diff(b), 5, "generator broke its own contract");

            c.swap(pos, pos + 1);
            let swapped: String = c.into_iter().collect();
            prop_assert!(
                is_valid_isbn13(&swapped),
                "expected mod-10 to be blind here; if this now fails the \
                 algorithm changed, it is not a bug in the test: {} -> {}", t, swapped
            );
        }

        #[test]
        fn arbitrary_input_never_panics(s in ".{0,64}") {
            let _ = normalize_isbn(&s);
        }

        #[test]
        fn arbitrary_input_that_parses_round_trips(s in "[0-9Xx: -]{0,32}") {
            if let Some(n) = normalize_isbn(&s) {
                let again = normalize_isbn(&n);
                prop_assert_eq!(again.as_deref(), Some(n.as_str()));
                prop_assert!(n.len() == 10 || n.len() == 13);
            }
        }
    }
}
