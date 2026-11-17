pub mod held;
pub mod query;
pub mod ranking;
pub mod session;
pub mod wellknown;
pub mod works;

mod correct;
mod kept;
pub(crate) mod local;
mod parsed;

use std::collections::HashMap;
use std::time::Duration;

use futures::future::join_all;
use strsim::jaro_winkler;

use crate::book::Book;
use crate::diagnostic::Diagnostic;
use crate::error::Result;
use crate::providers::{
    Manners, MetadataProvider, ProviderBook, ProviderId, ProviderWork, SearchRequest, WorkFacts,
};

/// Each provider's whole budget per search or lookup, rate-limit waits included; not configurable.
pub(crate) const PROVIDER_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn provider_deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + PROVIDER_TIMEOUT
}

/// Every provider call goes through here, so none can ask a provider that has said to wait.
pub(crate) async fn ask_provider<T>(
    p: &dyn MetadataProvider,
    deadline: tokio::time::Instant,
    call: impl std::future::Future<Output = Result<T>>,
) -> std::result::Result<T, Diagnostic> {
    let id = p.id();
    if let Some(left) = p.manners().and_then(Manners::closed_for) {
        return Err(Diagnostic::provider_rate_limited(id, left));
    }
    match tokio::time::timeout_at(deadline, call).await {
        Ok(Ok(found)) => Ok(found),
        Ok(Err(e)) => Err(match p.manners().and_then(Manners::closed_for) {
            Some(left) => Diagnostic::provider_rate_limited(id, left),
            None => Diagnostic::provider_failed(id, &e),
        }),
        Err(_) => Err(Diagnostic::provider_timed_out(id, PROVIDER_TIMEOUT)),
    }
}

pub(crate) fn free_text(req: &SearchRequest) -> Option<&str> {
    let only_text = req.title.is_none()
        && req.author.is_none()
        && req.publisher.is_none()
        && req.translator.is_none()
        && req.language.is_none()
        && req.year.is_none()
        && req.isbn.is_none();
    req.query
        .as_deref()
        .map(str::trim)
        .filter(|t| only_text && !t.is_empty())
}

#[derive(Debug, Default)]
pub struct IsbnLookup {
    pub book: Option<Book>,
    pub warnings: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct RankedResult {
    pub book: Book,
    pub sources: Vec<ProviderId>,
    pub score: f64,
    /// `None` from the federated search itself; `Engine::shelf_of` fills it by ISBN afterwards.
    pub shelf_id: Option<i64>,
    /// Which provider supplied each field of `book`, as the merge decided. Handing these back to
    /// `Engine::save_search_result` is what attributes a saved result.
    pub claims: Vec<FieldClaim>,
}

/// One field of a search result and the provider whose value it holds. `field` is a book column
/// name, the same vocabulary `Storage::field_provenance` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldClaim {
    pub field: String,
    pub provider: ProviderId,
}

#[derive(Debug, Default)]
pub struct SearchOutcome {
    pub results: Vec<RankedResult>,
    pub warnings: Vec<Diagnostic>,
}

impl SearchOutcome {
    /// A provider stood in for by kept records appears once, for its failure.
    pub fn failed_providers(&self) -> Vec<ProviderId> {
        use crate::diagnostic::DiagnosticKind as K;
        self.warnings
            .iter()
            .filter(|d| {
                !matches!(
                    d.kind,
                    K::ProviderAnsweredFromCache { .. } | K::KeptRecordUnreadable { .. }
                )
            })
            .filter_map(|d| d.provider())
            .collect()
    }

    pub fn timed_out(&self) -> bool {
        self.warnings.iter().any(|d| d.is_timeout())
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FieldClaims(Vec<(&'static str, ProviderId)>);

impl FieldClaims {
    /// Replaces any earlier claim: the merge overwrote the value.
    fn claim(&mut self, field: &'static str, provider: ProviderId) {
        match self.0.iter_mut().find(|(f, _)| *f == field) {
            Some(slot) => slot.1 = provider,
            None => self.0.push((field, provider)),
        }
    }

    /// Test-only: drift guard that these field names are columns the merge table governs.
    #[cfg(test)]
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&'static str, ProviderId)> + '_ {
        self.0.iter().copied()
    }

    pub(crate) fn source_of(&self, field: &str) -> Option<ProviderId> {
        self.0.iter().find(|(f, _)| *f == field).map(|(_, p)| *p)
    }

    pub(crate) fn to_vec(&self) -> Vec<FieldClaim> {
        self.0
            .iter()
            .map(|(field, provider)| FieldClaim {
                field: (*field).to_string(),
                provider: *provider,
            })
            .collect()
    }
}

pub async fn federated_search(
    providers: &[Box<dyn MetadataProvider>],
    req: &SearchRequest,
) -> Result<SearchOutcome> {
    let (results, warnings) = federated_search_claimed(providers, req).await?;
    Ok(SearchOutcome {
        results: results
            .into_iter()
            .map(|(r, claims)| RankedResult {
                claims: claims.to_vec(),
                ..r
            })
            .collect(),
        warnings,
    })
}

/// Each claim map is paired with its result, not a parallel vector, because the caller sorts them.
pub(crate) async fn federated_search_claimed(
    providers: &[Box<dyn MetadataProvider>],
    req: &SearchRequest,
) -> Result<(Vec<(RankedResult, FieldClaims)>, Vec<Diagnostic>)> {
    let fetches = providers.iter().map(|p| async {
        let id = p.id();
        match ask_provider(p.as_ref(), provider_deadline(), p.search(req)).await {
            Ok(books) => (id, None, Some(books)),
            Err(d) => (id, Some(d), None),
        }
    });
    let responses = futures::future::join_all(fetches).await;

    let mut warnings: Vec<Diagnostic> = Vec::new();
    let mut raw: Vec<ProviderBook> = Vec::new();
    for (id, diag, books) in responses {
        match (diag, books) {
            (_, Some(books)) => {
                tracing::debug!(provider = %id, count = books.len(), "provider answered");
                raw.extend(books);
            }
            (Some(d), None) => {
                // Logged beside the push so log and diagnostic cannot drift; detail only at `trace!` because a
                // provider's error text can carry the request URL, which carries the query.
                parsed::log_degraded(id, &d);
                warnings.push(d);
            }
            (None, None) => {}
        }
    }

    let mut merged = dedup(raw);
    for m in &mut merged {
        m.score = rank(&m.book, &m.sources, m.best_position, req);
    }
    merged.sort_by(|a, b| b.score.total_cmp(&a.score));
    let results = merged
        .into_iter()
        .map(|m| {
            (
                RankedResult {
                    book: m.book,
                    sources: m.sources,
                    score: m.score,
                    shelf_id: None,
                    claims: Vec::new(),
                },
                m.claims,
            )
        })
        .collect();
    Ok((results, warnings))
}

pub fn merge_provider_books(raw: Vec<ProviderBook>) -> Option<Book> {
    merge_provider_books_claimed(raw).map(|(book, _)| book)
}

pub(crate) fn merge_provider_books_claimed(raw: Vec<ProviderBook>) -> Option<(Book, FieldClaims)> {
    dedup(raw).into_iter().next().map(|m| (m.book, m.claims))
}

pub(crate) async fn lookup_isbn_all(
    providers: &[Box<dyn MetadataProvider>],
    isbn: &str,
) -> (Vec<ProviderBook>, Vec<Diagnostic>) {
    let asks = providers
        .iter()
        .map(|p| ask_provider(p.as_ref(), provider_deadline(), p.by_isbn(isbn)));
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    for (p, answer) in providers.iter().zip(join_all(asks).await) {
        match answer {
            Ok(Some(pb)) => found.push(pb),
            Ok(None) => {}
            Err(d) => {
                parsed::log_degraded(p.id(), &d);
                warnings.push(d);
            }
        }
    }
    (found, warnings)
}

#[derive(Debug)]
struct Merged {
    book: Book,
    sources: Vec<ProviderId>,
    best_position: usize,
    score: f64,
    /// Computed eagerly: a second traversal of the same merge decisions would drift.
    claims: FieldClaims,
    facts: WorkFacts,
}

fn merge_facts(into: &mut WorkFacts, from: WorkFacts) {
    into.edition_count = match (into.edition_count, from.edition_count) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    for t in from.also_titled {
        if !into.also_titled.contains(&t) {
            into.also_titled.push(t);
        }
    }
    if into.series.is_none() && from.series.is_some() {
        into.series = from.series;
        into.series_position = from.series_position;
    }
    if into.found_by_author.is_none() {
        into.found_by_author = from.found_by_author;
    }
    for l in from.languages {
        if !into.languages.contains(&l) {
            into.languages.push(l);
        }
    }
}

pub fn normalize(s: &str) -> String {
    let lowered = s.to_lowercase();
    let cleaned: String = lowered
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let mut words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.first().is_some_and(|w| is_article(w)) {
        words.remove(0);
    }
    words.join(" ")
}

/// Exactly one article is dropped: stripping repeatedly turns "The A Team" into "team" and changes
/// what the shared matcher treats as one book, so `normalize` is not idempotent on such titles.
fn is_article(word: &str) -> bool {
    matches!(word, "the" | "a" | "an")
}

#[cfg(test)]
fn starts_with_article(normalized: &str) -> bool {
    normalized
        .split(' ')
        .next()
        .is_some_and(|w| !w.is_empty() && is_article(w))
}

fn fingerprint(book: &Book) -> String {
    let title = normalize(book.title.as_deref().unwrap_or(""));
    let author = normalize(book.authors.first().map(String::as_str).unwrap_or(""));
    format!("{title}|{author}")
}

fn same_work(a: &Book, b: &Book) -> bool {
    let ta = normalize(a.title.as_deref().unwrap_or(""));
    let tb = normalize(b.title.as_deref().unwrap_or(""));
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    let title_sim = jaro_winkler(&ta, &tb);
    let author_sim = match (a.authors.first(), b.authors.first()) {
        (Some(x), Some(y)) => jaro_winkler(&normalize(x), &normalize(y)),
        _ => 1.0,
    };
    // Two volumes of a set are two works however alike the title; asked last, only for would-be merges.
    title_sim > 0.93
        && author_sim > 0.9
        && !crate::volumes::Volumes::of(&ta).differ(&crate::volumes::Volumes::of(&tb))
}

/// The merge itself is `MERGE_RULES`', so a new book column cannot be silently dropped here.
/// `claims` records who supplied each field, once, because only this place knows.
fn merge_into(a: &mut Book, b: &Book, b_provider: ProviderId, claims: &mut FieldClaims) {
    for field in crate::storage::merge_provider_record(a, b, b_provider.into()) {
        claims.claim(field, b_provider);
    }
}

fn dedup(raw: Vec<ProviderBook>) -> Vec<Merged> {
    dedup_works(raw.into_iter().map(ProviderWork::bare).collect())
}

fn dedup_works(raw: Vec<ProviderWork>) -> Vec<Merged> {
    let mut merged: Vec<Merged> = Vec::new();
    let mut by_isbn: HashMap<String, usize> = HashMap::new();
    let mut by_fingerprint: HashMap<String, usize> = HashMap::new();

    for ProviderWork {
        found: pb, facts, ..
    } in raw
    {
        let isbn_key = pb.book.canonical_isbn13();
        let existing = isbn_key
            .as_ref()
            .and_then(|k| by_isbn.get(k).copied())
            .or_else(|| {
                by_fingerprint
                    .get(&fingerprint(&pb.book))
                    .copied()
                    .filter(|&i| same_work(&merged[i].book, &pb.book))
            });

        match existing {
            Some(i) => {
                let Merged {
                    book,
                    claims,
                    facts: known,
                    ..
                } = &mut merged[i];
                merge_into(book, &pb.book, pb.provider, claims);
                merge_facts(known, facts);
                if !merged[i].sources.contains(&pb.provider) {
                    merged[i].sources.push(pb.provider);
                }
                merged[i].best_position = merged[i].best_position.min(pb.position);
                if let Some(k) = merged[i].book.canonical_isbn13() {
                    by_isbn.entry(k).or_insert(i);
                }
            }
            None => {
                let i = merged.len();
                if let Some(k) = isbn_key {
                    by_isbn.insert(k, i);
                }
                by_fingerprint.entry(fingerprint(&pb.book)).or_insert(i);
                // The merge table says what the first record speaks to; a list here could miss `cover_path`.
                let mut claims = FieldClaims::default();
                for field in crate::storage::fields_said(&pb.book) {
                    claims.claim(field, pb.provider);
                }
                merged.push(Merged {
                    book: pb.book,
                    sources: vec![pb.provider],
                    best_position: pb.position,
                    score: 0.0,
                    claims,
                    facts,
                });
            }
        }
    }
    merged
}

/// Weights: exact ISBN 1000, title 40, free query 30, author 25, publisher 10, translator 10,
/// year 5, language 5, provider position decay 8, found-in-both 6.
pub fn rank(book: &Book, sources: &[ProviderId], best_position: usize, req: &SearchRequest) -> f64 {
    let mut score = 0.0;

    if let Some(want) = &req.isbn
        && (book.isbn_10.as_deref() == Some(want.as_str())
            || book.isbn_13.as_deref() == Some(want.as_str()))
    {
        score += 1000.0;
    }
    if let (Some(want), Some(have)) = (&req.title, &book.title) {
        score += 40.0 * jaro_winkler(&normalize(want), &normalize(have));
    }
    if let (Some(q), Some(have)) = (&req.query, &book.title) {
        score += 30.0 * jaro_winkler(&normalize(q), &normalize(have));
    }
    if let Some(want) = &req.author {
        let best = book
            .authors
            .iter()
            .map(|a| jaro_winkler(&normalize(want), &normalize(a)))
            .fold(0.0_f64, f64::max);
        score += 25.0 * best;
    }
    if let (Some(want), Some(have)) = (&req.publisher, &book.publisher) {
        score += 10.0 * jaro_winkler(&normalize(want), &normalize(have));
    }
    if let Some(want) = &req.translator {
        let pool = book.translators.iter().chain(book.authors.iter());
        let best = pool
            .map(|a| jaro_winkler(&normalize(want), &normalize(a)))
            .fold(0.0_f64, f64::max);
        score += 10.0 * best;
    }
    if req.year.is_some() && book.publish_year == req.year {
        score += 5.0;
    }
    if let (Some(want), Some(have)) = (&req.language, &book.language)
        && want.eq_ignore_ascii_case(have)
    {
        score += 5.0;
    }
    score += 8.0 / (1.0 + best_position as f64);
    if sources.len() > 1 {
        score += 6.0;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pb(provider: ProviderId, position: usize, book: Book) -> ProviderBook {
        ProviderBook {
            book,
            provider,
            position,
        }
    }

    fn book(title: &str, author: &str) -> Book {
        Book {
            title: Some(title.into()),
            authors: vec![author.into()],
            ..Default::default()
        }
    }

    #[test]
    fn normalize_strips_articles_and_punct() {
        assert_eq!(normalize("The Trial"), "trial");
        assert_eq!(normalize("trial, the"), "trial the");
        assert_eq!(normalize("A  Wizard of Earthsea!"), "wizard of earthsea");
    }

    #[test]
    fn dedups_across_isbn_forms() {
        let mut a = book("Middlemarch", "George Eliot");
        a.isbn_10 = Some("1900000172".into());
        let mut b = book("Middlemarch", "George Eliot");
        b.isbn_13 = Some("9781900000178".into());
        b.description = Some("A saga.".into());

        let merged = dedup(vec![
            pb(ProviderId::OpenLibrary, 0, a),
            pb(ProviderId::GoogleBooks, 0, b),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].sources.len(), 2);
        assert_eq!(merged[0].book.description.as_deref(), Some("A saga."));
        assert_eq!(merged[0].book.isbn_10.as_deref(), Some("1900000172"));
    }

    #[test]
    fn dedups_by_fuzzy_fingerprint_without_isbn() {
        let a = book("The Trial", "Franz Kafka");
        let b = book("Trial", "Franz Kafka");
        let c = book("The Castle", "Franz Kafka");
        let merged = dedup(vec![
            pb(ProviderId::OpenLibrary, 0, a),
            pb(ProviderId::GoogleBooks, 0, b),
            pb(ProviderId::GoogleBooks, 1, c),
        ]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_provider_preferences() {
        let mut gb = book("Middlemarch", "George Eliot");
        gb.page_count = Some(512);
        gb.language = Some("en".into());
        let mut ol = book("Middlemarch", "George Eliot");
        ol.page_count = Some(490);

        let merged = dedup(vec![
            pb(ProviderId::GoogleBooks, 0, gb),
            pb(ProviderId::OpenLibrary, 0, ol),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].book.page_count, Some(490));
        assert_eq!(merged[0].book.language.as_deref(), Some("en"));
    }

    /// `merge_into`'s field names must be columns the merge table governs: `enrich_book_attributed`
    /// silently drops a claim it does not know.
    #[test]
    fn every_claimed_field_is_a_merge_column() {
        let columns: Vec<&str> = crate::storage::merge_columns().collect();
        let mut a = book("Middlemarch", "George Eliot");
        a.isbn_13 = Some("9781900000178".into());
        a.page_count = Some(490);
        a.description = Some("ol".into());
        a.publisher = Some("Blackwood".into());
        a.publish_year = Some(2017);
        a.first_published = Some(2017);
        a.first_sentence = Some("Miss Brooke had that kind of beauty which seems to be thrown into relief by poor dress.".into());
        a.cover_url = Some("https://example.invalid/c.jpg".into());
        a.openlibrary_key = Some("/works/OL1W".into());
        a.cover_path = Some("database/images/c.jpg".into());
        a.translators = vec!["A Translator".into()];
        a.subjects = vec!["Fiction / Literary".into()];
        a.series = Some("Dune".into());
        a.series_index = Some(2.0);
        let mut b = a.clone();
        b.googlebooks_id = Some("gb1".into());
        b.language = Some("en".into());
        b.isbn_10 = Some("1900000172".into());

        let merged = dedup(vec![
            pb(ProviderId::OpenLibrary, 0, a),
            pb(ProviderId::GoogleBooks, 0, b),
        ]);
        assert_eq!(merged.len(), 1);
        let claimed: Vec<&str> = merged[0].claims.iter().map(|(f, _)| f).collect();
        for field in &claimed {
            assert!(
                columns.contains(field),
                "{field} is claimed but is not a MERGE_RULES column: {columns:?}"
            );
        }
        for col in &columns {
            assert!(
                claimed.contains(col),
                "{col} was supplied but never claimed"
            );
        }
    }

    /// The pair moves as a unit: whoever supplies the series name supplies the index, even when none.
    #[test]
    fn a_series_index_never_arrives_without_its_series() {
        let mut named = book("Dune Messiah", "Frank Herbert");
        named.series = Some("Dune".into());
        let mut numbered = book("Dune Messiah", "Frank Herbert");
        numbered.series = Some("Dune Chronicles".into());
        numbered.series_index = Some(2.0);

        let merged = dedup(vec![
            pb(ProviderId::OpenLibrary, 0, named),
            pb(ProviderId::GoogleBooks, 0, numbered),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].book.series.as_deref(), Some("Dune"));
        assert_eq!(
            merged[0].book.series_index, None,
            "the index of a series that lost must not survive the merge"
        );
        assert!(
            !merged[0].claims.iter().any(|(f, _)| f == "series_index"),
            "an unfilled index must not be claimed"
        );
    }

    #[test]
    fn subject_sets_are_taken_whole_and_never_unioned() {
        let mut gb = book("Middlemarch", "George Eliot");
        gb.subjects = vec!["Fiction / Literary".into()];
        let mut ol = book("Middlemarch", "George Eliot");
        ol.subjects = vec!["Provincial England".into(), "Fiction".into()];

        let merged = dedup(vec![
            pb(ProviderId::GoogleBooks, 0, gb),
            pb(ProviderId::OpenLibrary, 0, ol),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].book.subjects, vec!["Fiction / Literary"]);
        assert_eq!(
            merged[0].claims.source_of("subjects"),
            Some(ProviderId::GoogleBooks)
        );
    }

    #[test]
    fn a_claim_names_whoever_won_the_field_not_whoever_arrived_first() {
        let mut ol = book("Middlemarch", "George Eliot");
        ol.page_count = Some(490);
        ol.description = Some("the openlibrary blurb".into());
        let mut gb = book("Middlemarch", "George Eliot");
        gb.page_count = Some(512);
        gb.description = Some("the google books blurb".into());

        let merged = dedup(vec![
            pb(ProviderId::OpenLibrary, 0, ol.clone()),
            pb(ProviderId::GoogleBooks, 0, gb.clone()),
        ]);
        assert_eq!(
            merged[0].book.description.as_deref(),
            Some("the google books blurb")
        );
        assert_eq!(
            merged[0].claims.source_of("description"),
            Some(ProviderId::GoogleBooks)
        );
        assert_eq!(
            merged[0].claims.source_of("page_count"),
            Some(ProviderId::OpenLibrary)
        );

        let merged = dedup(vec![
            pb(ProviderId::GoogleBooks, 0, gb),
            pb(ProviderId::OpenLibrary, 0, ol),
        ]);
        assert_eq!(merged[0].book.page_count, Some(490));
        assert_eq!(
            merged[0].claims.source_of("page_count"),
            Some(ProviderId::OpenLibrary)
        );
        assert_eq!(
            merged[0].claims.source_of("description"),
            Some(ProviderId::GoogleBooks)
        );
    }

    #[test]
    fn exact_isbn_dominates_ranking() {
        let req = SearchRequest {
            isbn: Some("9781900000178".into()),
            title: Some("Middlemarch".into()),
            ..Default::default()
        };
        let mut with_isbn = book("Some Other Title", "Nobody");
        with_isbn.isbn_13 = Some("9781900000178".into());
        let perfect_title = book("Middlemarch", "George Eliot");

        let s1 = rank(&with_isbn, &[ProviderId::OpenLibrary], 5, &req);
        let s2 = rank(&perfect_title, &[ProviderId::OpenLibrary], 0, &req);
        assert!(s1 > s2);
    }

    #[test]
    fn title_match_beats_author_match() {
        let req = SearchRequest {
            title: Some("Middlemarch".into()),
            author: Some("George Eliot".into()),
            ..Default::default()
        };
        let title_hit = book("Middlemarch", "Someone Else");
        let author_hit = book("Silas Marner", "George Eliot");
        let s_title = rank(&title_hit, &[ProviderId::OpenLibrary], 0, &req);
        let s_author = rank(&author_hit, &[ProviderId::OpenLibrary], 0, &req);
        assert!(s_title > s_author);
    }

    #[test]
    fn both_provider_bonus_and_position_decay() {
        let req = SearchRequest {
            title: Some("Dune".into()),
            ..Default::default()
        };
        let b = book("Dune", "Frank Herbert");
        let one = rank(&b, &[ProviderId::OpenLibrary], 0, &req);
        let both = rank(
            &b,
            &[ProviderId::OpenLibrary, ProviderId::GoogleBooks],
            0,
            &req,
        );
        let deep = rank(&b, &[ProviderId::OpenLibrary], 20, &req);
        assert!(both > one);
        assert!(one > deep);
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    /// Sum of every non-ISBN ceiling: title 40, query 30, author 25, publisher 10, translator 10,
    /// year 5, language 5, position 8, multi-source 6.
    const MAX_NON_ISBN_SCORE: f64 = 40.0 + 30.0 + 25.0 + 10.0 + 10.0 + 5.0 + 5.0 + 8.0 + 6.0;

    fn text() -> impl Strategy<Value = String> {
        // Includes punctuation, accents and CJK: `normalize` and jaro_winkler must cope with any text.
        prop_oneof![
            "[a-zA-Z ]{0,20}",
            "[\\p{Latin}\\p{Han}\\p{Hiragana}'’,.:!-]{0,20}",
            Just(String::new()),
        ]
    }

    fn arb_book() -> impl Strategy<Value = Book> {
        (
            proptest::option::of(text()),
            proptest::collection::vec(text(), 0..3),
            proptest::option::of(text()),
            proptest::option::of(1000i64..2100),
            proptest::option::of("[a-z]{2}"),
        )
            .prop_map(|(title, authors, publisher, publish_year, language)| Book {
                title,
                authors,
                publisher,
                publish_year,
                language,
                ..Default::default()
            })
    }

    fn arb_req() -> impl Strategy<Value = SearchRequest> {
        (
            proptest::option::of(text()),
            proptest::option::of(text()),
            proptest::option::of(text()),
            proptest::option::of(1000i64..2100),
        )
            .prop_map(|(query, title, author, year)| SearchRequest {
                query,
                title,
                author,
                year,
                ..Default::default()
            })
    }

    proptest! {
        /// `total_cmp` orders NaN by bit pattern without panicking, so a NaN silently scrambles results.
        #[test]
        fn rank_is_always_finite(
            book in arb_book(),
            req in arb_req(),
            pos in 0usize..1000,
            two_sources in any::<bool>(),
        ) {
            let sources: Vec<ProviderId> = if two_sources {
                vec![ProviderId::OpenLibrary, ProviderId::GoogleBooks]
            } else {
                vec![ProviderId::OpenLibrary]
            };
            let s = rank(&book, &sources, pos, &req);
            prop_assert!(s.is_finite(), "non-finite score {} for {:?}", s, book.title);
            prop_assert!(s >= 0.0, "negative score {}", s);
        }

        #[test]
        fn an_exact_isbn_match_always_wins(
            other in arb_book(),
            req_extra in arb_req(),
            pos_a in 0usize..50,
            pos_b in 0usize..50,
        ) {
            let isbn = "9780316228534";
            let mut req = req_extra;
            req.isbn = Some(isbn.to_string());

            let matching = Book {
                isbn_13: Some(isbn.to_string()),
                ..Default::default()
            };
            let mut rival = other;
            rival.isbn_13 = None;
            rival.isbn_10 = None;

            let hit = rank(&matching, &[ProviderId::OpenLibrary], pos_a, &req);
            let miss = rank(
                &rival,
                &[ProviderId::OpenLibrary, ProviderId::GoogleBooks],
                pos_b,
                &req,
            );

            prop_assert!(miss <= MAX_NON_ISBN_SCORE, "score model changed: {}", miss);
            prop_assert!(
                hit > miss,
                "ISBN match ({}) did not outrank a non-match ({})", hit, miss
            );
        }

        #[test]
        fn earlier_positions_never_score_lower(
            book in arb_book(),
            req in arb_req(),
            a in 0usize..500,
            b in 0usize..500,
        ) {
            prop_assume!(a < b);
            let src = [ProviderId::OpenLibrary];
            prop_assert!(rank(&book, &src, a, &req) >= rank(&book, &src, b, &req));
        }

        #[test]
        fn agreement_between_providers_never_hurts(book in arb_book(), req in arb_req()) {
            let one = rank(&book, &[ProviderId::OpenLibrary], 3, &req);
            let two = rank(
                &book,
                &[ProviderId::OpenLibrary, ProviderId::GoogleBooks],
                3,
                &req,
            );
            prop_assert!(two >= one);
        }

        /// Idempotence holds only when the output does not itself begin with an article.
        #[test]
        fn normalize_strips_one_article_and_is_otherwise_well_shaped(s in text()) {
            let once = normalize(&s);
            if !starts_with_article(&once) {
                prop_assert_eq!(normalize(&once), once.clone());
            } else {
                prop_assert_eq!(
                    normalize(&once),
                    once.split_once(' ').map(|(_, rest)| rest).unwrap_or("")
                );
            }
            prop_assert!(!once.starts_with(' ') && !once.ends_with(' '), "{:?}", once);
            prop_assert!(!once.contains("  "), "double space in {:?}", once);
            prop_assert!(
                once.chars().all(|c| c.is_alphanumeric() || c == ' '),
                "unexpected char in {:?}", once
            );
        }

        /// Symmetry keeps dedup independent of provider order; transitivity is deliberately not asserted.
        #[test]
        fn same_work_is_symmetric(a in arb_book(), b in arb_book()) {
            prop_assert_eq!(same_work(&a, &b), same_work(&b, &a));
        }

        #[test]
        fn dedup_never_grows_and_never_empties(
            books in proptest::collection::vec(arb_book(), 0..8),
        ) {
            let raw: Vec<ProviderBook> = books
                .into_iter()
                .enumerate()
                .map(|(i, book)| ProviderBook {
                    book,
                    provider: if i % 2 == 0 {
                        ProviderId::OpenLibrary
                    } else {
                        ProviderId::GoogleBooks
                    },
                    position: i,
                })
                .collect();
            let n = raw.len();
            let out = dedup(raw);
            prop_assert!(out.len() <= n);
            prop_assert_eq!(out.is_empty(), n == 0);
        }

        /// Scoped to the `prefer` fields: `fill` fields are first-wins and order-dependent by design.
        #[test]
        fn dedup_identity_is_permutation_invariant(
            books in proptest::collection::vec(arb_book(), 1..6),
        ) {
            let mk = |order: Vec<Book>| -> Vec<ProviderBook> {
                order
                    .into_iter()
                    .enumerate()
                    .map(|(i, book)| ProviderBook {
                        book,
                        provider: ProviderId::OpenLibrary,
                        position: i,
                    })
                    .collect()
            };
            let forward = dedup(mk(books.clone()));
            let mut reversed_input = books.clone();
            reversed_input.reverse();
            let backward = dedup(mk(reversed_input));

            prop_assert_eq!(
                forward.len(),
                backward.len(),
                "reordering changed how many distinct works were found"
            );

            let mut a: Vec<Option<String>> =
                forward.iter().map(|m| m.book.isbn_13.clone()).collect();
            let mut b: Vec<Option<String>> =
                backward.iter().map(|m| m.book.isbn_13.clone()).collect();
            a.sort();
            b.sort();
            prop_assert_eq!(a, b, "reordering changed the set of ISBNs");
        }
    }
}

#[cfg(test)]
mod fanout_tests {
    use super::*;
    use crate::error::EngineError;
    use async_trait::async_trait;

    enum Behaviour {
        Answers(Vec<Book>),
        Fails(String),
        Hangs,
    }

    struct FakeProvider {
        id: ProviderId,
        behaviour: Behaviour,
    }

    #[async_trait]
    impl MetadataProvider for FakeProvider {
        fn id(&self) -> ProviderId {
            self.id
        }
        async fn search(&self, _req: &SearchRequest) -> Result<Vec<ProviderBook>> {
            match &self.behaviour {
                Behaviour::Answers(books) => Ok(books
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(position, book)| ProviderBook {
                        book,
                        provider: self.id,
                        position,
                    })
                    .collect()),
                Behaviour::Fails(message) => Err(EngineError::Provider {
                    provider: self.id,
                    message: message.clone(),
                }),
                Behaviour::Hangs => {
                    tokio::time::sleep(PROVIDER_TIMEOUT * 2).await;
                    Ok(Vec::new())
                }
            }
        }
        async fn by_isbn(&self, _isbn: &str) -> Result<Option<ProviderBook>> {
            Ok(None)
        }
    }

    fn provider(id: ProviderId, behaviour: Behaviour) -> Box<dyn MetadataProvider> {
        Box::new(FakeProvider { id, behaviour })
    }

    fn book(title: &str) -> Book {
        Book {
            title: Some(title.into()),
            authors: vec!["Someone".into()],
            ..Default::default()
        }
    }

    fn req() -> SearchRequest {
        SearchRequest {
            query: Some("dune".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn one_provider_failing_does_not_lose_the_other_s_results() {
        let providers = vec![
            provider(
                ProviderId::OpenLibrary,
                Behaviour::Fails("upstream exploded".into()),
            ),
            provider(
                ProviderId::GoogleBooks,
                Behaviour::Answers(vec![book("Dune"), book("Dune Messiah")]),
            ),
        ];
        let out = federated_search(&providers, &req()).await.unwrap();

        assert_eq!(
            out.results.len(),
            2,
            "surviving provider's results were lost"
        );
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.failed_providers(), vec![ProviderId::OpenLibrary]);
        assert!(!out.timed_out(), "a failure is not a timeout");
    }

    #[tokio::test(start_paused = true)]
    async fn a_hanging_provider_times_out_and_says_so() {
        let started = std::time::Instant::now();
        let providers = vec![
            provider(ProviderId::OpenLibrary, Behaviour::Hangs),
            provider(
                ProviderId::GoogleBooks,
                Behaviour::Answers(vec![book("Dune")]),
            ),
        ];
        let out = federated_search(&providers, &req()).await.unwrap();

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "test actually waited {:?}; paused time is not in effect",
            started.elapsed()
        );
        assert_eq!(out.results.len(), 1);
        assert!(out.timed_out());
        assert_eq!(out.failed_providers(), vec![ProviderId::OpenLibrary]);
        assert_eq!(out.warnings[0].to_string(), "openlibrary: timed out");
    }

    #[tokio::test(start_paused = true)]
    async fn every_provider_down_is_still_ok_with_two_diagnostics() {
        let providers = vec![
            provider(ProviderId::OpenLibrary, Behaviour::Hangs),
            provider(
                ProviderId::GoogleBooks,
                Behaviour::Fails("upstream exploded".into()),
            ),
        ];
        let out = federated_search(&providers, &req())
            .await
            .expect("total provider failure must not be an Err");

        assert!(out.results.is_empty());
        assert_eq!(out.warnings.len(), 2);
        let mut failed = out.failed_providers();
        failed.sort();
        assert_eq!(
            failed,
            vec![ProviderId::OpenLibrary, ProviderId::GoogleBooks]
        );
    }

    #[tokio::test]
    async fn both_answering_dedups_and_credits_both_sources() {
        let providers = vec![
            provider(
                ProviderId::OpenLibrary,
                Behaviour::Answers(vec![book("Dune")]),
            ),
            provider(
                ProviderId::GoogleBooks,
                Behaviour::Answers(vec![book("Dune")]),
            ),
        ];
        let out = federated_search(&providers, &req()).await.unwrap();

        assert!(out.warnings.is_empty());
        assert_eq!(out.results.len(), 1, "the same work was not deduped");
        assert_eq!(
            out.results[0].sources.len(),
            2,
            "second source not credited"
        );
    }

    #[tokio::test]
    async fn a_quota_refusal_stays_recognisable_as_a_rate_limit() {
        let providers = vec![provider(
            ProviderId::GoogleBooks,
            Behaviour::Fails("HTTP 429 rate limit exceeded".into()),
        )];
        let out = federated_search(&providers, &req()).await.unwrap();

        assert_eq!(out.warnings.len(), 1);
        assert!(
            out.warnings[0].is_rate_limited(),
            "quota refusal lost its classification: {:?}",
            out.warnings[0]
        );
        assert!(!out.warnings[0].is_timeout());
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_provider_is_not_asked_and_is_reported() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Closed {
            asked: Arc<AtomicUsize>,
            manners: Manners,
        }
        #[async_trait]
        impl MetadataProvider for Closed {
            fn id(&self) -> ProviderId {
                ProviderId::OpenLibrary
            }
            async fn search(&self, _req: &SearchRequest) -> Result<Vec<ProviderBook>> {
                self.asked.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            }
            async fn by_isbn(&self, _isbn: &str) -> Result<Option<ProviderBook>> {
                self.asked.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            }
            fn manners(&self) -> Option<&Manners> {
                Some(&self.manners)
            }
        }

        let asked = Arc::new(AtomicUsize::new(0));
        let manners = Manners::unlimited();
        manners.close_for(Duration::from_secs(90));
        let providers: Vec<Box<dyn MetadataProvider>> = vec![
            Box::new(Closed {
                asked: asked.clone(),
                manners,
            }),
            provider(
                ProviderId::GoogleBooks,
                Behaviour::Answers(vec![book("Dune")]),
            ),
        ];
        let out = federated_search(&providers, &req()).await.unwrap();

        assert_eq!(
            asked.load(Ordering::SeqCst),
            0,
            "a closed provider was asked"
        );
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].is_rate_limited());
        assert_eq!(
            out.warnings[0].to_string(),
            "openlibrary: asked to wait; not asked again for 90s"
        );
    }

    #[tokio::test]
    async fn no_providers_at_all_is_an_empty_success() {
        let out = federated_search(&[], &req()).await.unwrap();
        assert!(out.results.is_empty() && out.warnings.is_empty());
    }

    #[tokio::test]
    async fn results_are_sorted_by_descending_score() {
        let providers = vec![provider(
            ProviderId::OpenLibrary,
            Behaviour::Answers(vec![book("Something Else"), book("Dune"), book("Dune Two")]),
        )];
        let out = federated_search(&providers, &req()).await.unwrap();
        let scores: Vec<f64> = out.results.iter().map(|r| r.score).collect();
        assert!(
            scores.windows(2).all(|w| w[0] >= w[1]),
            "not descending: {scores:?}"
        );
    }
}
