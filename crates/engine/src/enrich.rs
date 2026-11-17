//! Looking a book up again: provider enrichment as an explicit per-book action, matched through
//! `crate::matching`; deliberately no bulk entry point, since each book costs a provider fan-out.
//! Below `AUTO_MATCH` nothing is written and candidates come back; a `user`-provenance field is
//! never overwritten.

use std::collections::HashSet;

use crate::book::Book;
use crate::diagnostic::Diagnostic;
use crate::error::{EngineError, Result};
use crate::matching::{CANDIDATE_MIN, Prepared, Query};
use crate::providers::{MetadataProvider, ProviderBook, ProviderId, SearchRequest};
use crate::search::{self, FieldClaims};
use crate::storage::{Source, Storage, field_pair, field_value, merge_columns};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EnrichMatch {
    /// The book's own ISBN: identity, so no candidates are offered.
    Isbn,
    Title(f64),
}

#[derive(Debug, Clone)]
pub struct EnrichCandidate {
    pub book: Book,
    pub score: f64,
    pub sources: Vec<ProviderId>,
}

#[derive(Debug, Clone)]
pub enum EnrichOutcome {
    Enriched(EnrichMatch),
    /// Nothing was written. `candidates` is empty when results came back but none resembled this book.
    Refused { candidates: Vec<EnrichCandidate> },
    NothingFound,
    /// Nothing came back and some provider failed or timed out. Never reported as "no provider knows
    /// this book".
    NoAnswer,
    Unaskable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldChange {
    pub field: &'static str,
    /// `None` when the field moved but no provider claimed it.
    pub source: Option<Source>,
    /// `None` means the field was empty.
    pub before: Option<String>,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeldField {
    pub field: &'static str,
    pub offered: String,
    pub offered_by: Option<Source>,
    pub kept: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EnrichReport {
    pub book_id: i64,
    pub outcome: EnrichOutcome,
    pub filled: Vec<FieldChange>,
    pub held: Vec<HeldField>,
    /// `None` both when the book already had a cover and when no record offered a URL.
    pub cover: Option<std::path::PathBuf>,
    pub warnings: Vec<Diagnostic>,
}

impl EnrichReport {
    fn empty(book_id: i64, outcome: EnrichOutcome) -> Self {
        Self {
            book_id,
            outcome,
            filled: Vec::new(),
            held: Vec::new(),
            cover: None,
            warnings: Vec::new(),
        }
    }

    /// `filled` alone under-reports: a run that only downloaded a cover changed the book.
    pub fn wrote_anything(&self) -> bool {
        !self.filled.is_empty() || self.cover.is_some()
    }

    pub(crate) fn source_of(&self, field: &str) -> Option<Source> {
        self.filled
            .iter()
            .find(|c| c.field == field)
            .and_then(|c| c.source)
    }
}

/// Keeps each provider's answer; `Engine::lookup_isbn` merges and discards the per-field
/// attribution this needs.
pub(crate) async fn lookup_isbn_raw(
    providers: &[Box<dyn MetadataProvider>],
    isbn: &str,
) -> (Vec<ProviderBook>, Vec<Diagnostic>) {
    search::lookup_isbn_all(providers, isbn).await
}

/// The cover download is not here: it needs the facade's HTTP client and images directory.
pub(crate) async fn enrich_metadata(
    storage: &Storage,
    providers: &[Box<dyn MetadataProvider>],
    book_id: i64,
) -> Result<EnrichReport> {
    let before = storage
        .get_book(book_id)
        .await?
        .ok_or_else(|| EngineError::NotFound(format!("book id {book_id}")))?;

    // No fallback from ISBN to a title search: that would answer for some edition and overwrite
    // edition-specific fields.
    let isbn = before
        .isbn_13
        .as_deref()
        .or(before.isbn_10.as_deref())
        .map(str::to_string);

    let (found, mut warnings, outcome) = match isbn {
        Some(isbn) => {
            let (raw, warnings) = lookup_isbn_raw(providers, &isbn).await;
            match search::merge_provider_books_claimed(raw) {
                Some(found) => (
                    Some(found),
                    warnings,
                    EnrichOutcome::Enriched(EnrichMatch::Isbn),
                ),
                None => {
                    let outcome = silence(&warnings);
                    return Ok(EnrichReport {
                        warnings,
                        ..EnrichReport::empty(book_id, outcome)
                    });
                }
            }
        }
        None => match by_title(providers, &before).await? {
            TitleSearch::Unaskable => {
                return Ok(EnrichReport::empty(book_id, EnrichOutcome::Unaskable));
            }
            TitleSearch::Refused {
                candidates,
                any_results,
                warnings,
            } => {
                let outcome = if any_results {
                    EnrichOutcome::Refused { candidates }
                } else {
                    silence(&warnings)
                };
                return Ok(EnrichReport {
                    warnings,
                    ..EnrichReport::empty(book_id, outcome)
                });
            }
            TitleSearch::Matched {
                found,
                score,
                warnings,
            } => (
                Some(found),
                warnings,
                EnrichOutcome::Enriched(EnrichMatch::Title(score)),
            ),
        },
    };

    let Some((record, claims)) = found else {
        return Ok(EnrichReport {
            warnings,
            ..EnrichReport::empty(book_id, EnrichOutcome::NothingFound)
        });
    };

    // Read before the write: the guard is in the generated SQL, and this half only reports it.
    let owned: HashSet<String> = storage
        .field_provenance(book_id)
        .await?
        .into_iter()
        .filter(|f| f.source == Source::User.as_str())
        .map(|f| f.field)
        .collect();

    let mut held = Vec::new();
    let mut claim_list: Vec<(&'static str, Source)> = Vec::new();
    for field in merge_columns() {
        let Some(offered) = field_value(&record, field) else {
            continue;
        };
        if let Some(source) = claims.source_of(field) {
            // Every claim goes through, user-owned fields included: `stamp` already refuses them, and filtering
            // here would respell the guard.
            claim_list.push((field, source.into()));
        }
        let kept = field_value(&before, field);
        // Owning half a pair holds the whole pair (`Rule::pair`); the report must read ownership as the
        // merge does.
        let held_back =
            owned.contains(field) || field_pair(field).is_some_and(|p| owned.contains(p));
        if held_back && kept.as_deref() != Some(offered.as_str()) {
            held.push(HeldField {
                field,
                offered,
                offered_by: claims.source_of(field).map(Source::from),
                kept,
            });
        }
    }

    storage
        .enrich_book_attributed(book_id, &record, &claim_list)
        .await?;

    let after = storage
        .get_book(book_id)
        .await?
        .ok_or_else(|| EngineError::NotFound(format!("book id {book_id}")))?;
    let filled = changes(&before, &after, &claims);

    warnings.shrink_to_fit();
    Ok(EnrichReport {
        book_id,
        outcome,
        filled,
        held,
        cover: None,
        warnings,
    })
}

/// "No provider knows this book" may be said only when every provider answered.
fn silence(warnings: &[Diagnostic]) -> EnrichOutcome {
    if warnings.is_empty() {
        EnrichOutcome::NothingFound
    } else {
        EnrichOutcome::NoAnswer
    }
}

fn changes(before: &Book, after: &Book, claims: &FieldClaims) -> Vec<FieldChange> {
    merge_columns()
        .filter_map(|field| {
            let before_value = field_value(before, field);
            let after_value = field_value(after, field)?;
            if before_value.as_deref() == Some(after_value.as_str()) {
                return None;
            }
            Some(FieldChange {
                field,
                source: claims.source_of(field).map(Source::from),
                before: before_value,
                after: after_value,
            })
        })
        .collect()
}

/// `large_enum_variant` is allowed: one value is built per call and destructured at once, so a
/// `Box` buys nothing.
#[allow(clippy::large_enum_variant)]
enum TitleSearch {
    Unaskable,
    Refused {
        candidates: Vec<EnrichCandidate>,
        any_results: bool,
        warnings: Vec<Diagnostic>,
    },
    Matched {
        found: (Book, FieldClaims),
        score: f64,
        warnings: Vec<Diagnostic>,
    },
}

/// Only the first author goes in the request: `SearchRequest::author` is one field, and joining
/// several scores worse
/// against every result. The matcher's veto still sees the whole list.
async fn by_title(providers: &[Box<dyn MetadataProvider>], before: &Book) -> Result<TitleSearch> {
    let query = Query::new(before.title.as_deref(), &before.authors);
    let Some(prepared) = Prepared::new(&query) else {
        return Ok(TitleSearch::Unaskable);
    };

    let req = SearchRequest {
        title: before.title.clone(),
        author: before.authors.first().cloned(),
        ..Default::default()
    };
    let (results, warnings) = search::federated_search_claimed(providers, &req).await?;
    let any_results = !results.is_empty();
    // `compare` returning `None` means a coincidence of letters: absent, not a low-scoring candidate.
    let mut scored: Vec<(usize, f64, bool)> = results
        .iter()
        .enumerate()
        .filter_map(|(i, (r, _))| prepared.compare(&r.book).map(|v| (i, v.score, v.can_auto)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

    let mut results: Vec<Option<(search::RankedResult, FieldClaims)>> =
        results.into_iter().map(Some).collect();
    let take = |results: &mut Vec<Option<(search::RankedResult, FieldClaims)>>, i: usize| {
        results[i].take().expect("each index is visited once")
    };

    if let Some(&(i, score, true)) = scored.first() {
        let (result, claims) = take(&mut results, i);
        return Ok(TitleSearch::Matched {
            found: (result.book, claims),
            score,
            warnings,
        });
    }

    let candidates = scored
        .iter()
        .filter(|(_, score, can_auto)| !can_auto && *score >= CANDIDATE_MIN)
        .map(|&(i, score, _)| {
            let (result, _) = take(&mut results, i);
            EnrichCandidate {
                book: result.book,
                score,
                sources: result.sources,
            }
        })
        .collect();

    Ok(TitleSearch::Refused {
        candidates,
        any_results,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct Fake {
        id: ProviderId,
        by_isbn: Option<Book>,
        results: Vec<Book>,
        fails: bool,
    }

    impl Fake {
        fn new(id: ProviderId) -> Self {
            Fake {
                id,
                by_isbn: None,
                results: Vec::new(),
                fails: false,
            }
        }
        fn isbn(mut self, book: Book) -> Self {
            self.by_isbn = Some(book);
            self
        }
        fn finds(mut self, books: Vec<Book>) -> Self {
            self.results = books;
            self
        }
        fn broken(mut self) -> Self {
            self.fails = true;
            self
        }
        fn boxed(self) -> Box<dyn MetadataProvider> {
            Box::new(self)
        }
    }

    #[async_trait]
    impl MetadataProvider for Fake {
        fn id(&self) -> ProviderId {
            self.id
        }
        async fn search(&self, _req: &SearchRequest) -> Result<Vec<ProviderBook>> {
            if self.fails {
                return Err(EngineError::Provider {
                    provider: self.id,
                    message: "upstream exploded".into(),
                });
            }
            Ok(self
                .results
                .iter()
                .cloned()
                .enumerate()
                .map(|(position, book)| ProviderBook {
                    book,
                    provider: self.id,
                    position,
                })
                .collect())
        }
        async fn by_isbn(&self, _isbn: &str) -> Result<Option<ProviderBook>> {
            if self.fails {
                return Err(EngineError::Provider {
                    provider: self.id,
                    message: "upstream exploded".into(),
                });
            }
            Ok(self.by_isbn.clone().map(|book| ProviderBook {
                book,
                provider: self.id,
                position: 0,
            }))
        }
    }

    async fn sidecar_seeded(title: &str, authors: &[&str]) -> (Storage, i64) {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s
            .upsert_book(
                &Book {
                    title: Some(title.into()),
                    authors: authors.iter().map(|a| (*a).to_string()).collect(),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        (s, id)
    }

    fn record(title: &str, authors: &[&str]) -> Book {
        Book {
            title: Some(title.into()),
            authors: authors.iter().map(|a| (*a).to_string()).collect(),
            ..Default::default()
        }
    }

    fn filled(report: &EnrichReport, field: &str) -> Option<String> {
        report
            .filled
            .iter()
            .find(|c| c.field == field)
            .map(|c| c.after.clone())
    }

    #[tokio::test]
    async fn each_field_is_attributed_to_the_provider_that_actually_supplied_it() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    isbn_13: Some("9781900000178".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();

        let providers = vec![
            Fake::new(ProviderId::OpenLibrary)
                .isbn(Book {
                    page_count: Some(490),
                    description: Some("the openlibrary blurb".into()),
                    publisher: Some("Blackwood".into()),
                    ..record("Middlemarch", &["George Eliot"])
                })
                .boxed(),
            Fake::new(ProviderId::GoogleBooks)
                .isbn(Book {
                    page_count: Some(512),
                    description: Some("the google books blurb".into()),
                    language: Some("en".into()),
                    ..record("Middlemarch", &["George Eliot"])
                })
                .boxed(),
        ];

        let report = enrich_metadata(&s, &providers, id).await.unwrap();
        assert!(matches!(
            report.outcome,
            EnrichOutcome::Enriched(EnrichMatch::Isbn)
        ));

        let book = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(
            book.page_count,
            Some(490),
            "OpenLibrary wins the page count"
        );
        assert_eq!(
            book.description.as_deref(),
            Some("the google books blurb"),
            "Google Books wins the description"
        );

        assert_eq!(
            s.field_source(id, "page_count").await.unwrap().as_deref(),
            Some("openlibrary")
        );
        assert_eq!(
            s.field_source(id, "description").await.unwrap().as_deref(),
            Some("googlebooks")
        );
        assert_eq!(
            s.field_source(id, "language").await.unwrap().as_deref(),
            Some("googlebooks")
        );
        assert_eq!(
            s.field_source(id, "publisher").await.unwrap().as_deref(),
            Some("openlibrary")
        );

        assert_eq!(filled(&report, "page_count").as_deref(), Some("490"));
        let page = report
            .filled
            .iter()
            .find(|c| c.field == "page_count")
            .unwrap();
        assert_eq!(page.source, Some(Source::OpenLibrary));
        assert_eq!(page.before, None, "the field was empty");
    }

    #[tokio::test]
    async fn a_book_with_no_isbn_is_matched_by_title_and_enriched() {
        let (s, id) = sidecar_seeded("Middlemarch", &["George Eliot"]).await;
        let providers = vec![
            Fake::new(ProviderId::OpenLibrary)
                .finds(vec![Book {
                    page_count: Some(490),
                    isbn_13: Some("9781900000178".into()),
                    ..record("Middlemarch", &["George Eliot"])
                }])
                .boxed(),
        ];

        let report = enrich_metadata(&s, &providers, id).await.unwrap();
        match report.outcome {
            EnrichOutcome::Enriched(EnrichMatch::Title(score)) => {
                assert!(score >= crate::matching::AUTO_MATCH, "score {score}")
            }
            other => panic!("expected a title match, got {other:?}"),
        }
        let book = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(book.page_count, Some(490));
        assert_eq!(book.isbn_13.as_deref(), Some("9781900000178"));
    }

    #[tokio::test]
    async fn a_near_miss_writes_nothing_and_comes_back_as_a_candidate() {
        let (s, id) = sidecar_seeded("Middlemarch", &["George Eliot"]).await;
        let providers = vec![
            Fake::new(ProviderId::OpenLibrary)
                .finds(vec![Book {
                    page_count: Some(490),
                    isbn_13: Some("9781900000178".into()),
                    ..record("Middlemarch: A Study of Provincial Life", &["George Eliot"])
                }])
                .boxed(),
        ];

        let report = enrich_metadata(&s, &providers, id).await.unwrap();
        match &report.outcome {
            EnrichOutcome::Refused { candidates } => {
                assert_eq!(candidates.len(), 1);
                assert!(candidates[0].score >= CANDIDATE_MIN);
                assert!(candidates[0].score < crate::matching::AUTO_MATCH);
                assert_eq!(
                    candidates[0].book.isbn_13.as_deref(),
                    Some("9781900000178"),
                    "the candidate carries the durable name the next move uses"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(report.filled.is_empty());
        let book = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(book.page_count, None, "a refusal writes nothing");
        assert!(s.field_provenance(id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unrelated_result_is_not_offered_at_all() {
        let (s, id) = sidecar_seeded("Middlemarch", &["George Eliot"]).await;
        let providers = vec![
            Fake::new(ProviderId::OpenLibrary)
                .finds(vec![record("Middlesex", &["Jeffrey Eugenides"])])
                .boxed(),
        ];

        let report = enrich_metadata(&s, &providers, id).await.unwrap();
        match &report.outcome {
            EnrichOutcome::Refused { candidates } => assert!(candidates.is_empty()),
            other => panic!("expected an empty refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn nothing_found_is_not_a_refusal() {
        let (s, id) = sidecar_seeded("An Unpublished Manuscript", &[]).await;
        let providers = vec![Fake::new(ProviderId::OpenLibrary).boxed()];
        let report = enrich_metadata(&s, &providers, id).await.unwrap();
        assert!(matches!(report.outcome, EnrichOutcome::NothingFound));

        let s2 = Storage::connect("sqlite::memory:").await.unwrap();
        let id2 = s2
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    isbn_13: Some("9781900000178".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let providers = vec![
            Fake::new(ProviderId::OpenLibrary)
                .finds(vec![record("Middlemarch", &["George Eliot"])])
                .boxed(),
        ];
        let report = enrich_metadata(&s2, &providers, id2).await.unwrap();
        assert!(
            matches!(report.outcome, EnrichOutcome::NothingFound),
            "an unknown ISBN must not fall through to the title search: {:?}",
            report.outcome
        );
    }

    #[tokio::test]
    async fn every_provider_falling_over_is_no_answer_not_nothing_found() {
        let (s, id) = sidecar_seeded("Middlemarch", &["George Eliot"]).await;
        let both_down = vec![
            Fake::new(ProviderId::OpenLibrary).broken().boxed(),
            Fake::new(ProviderId::GoogleBooks).broken().boxed(),
        ];
        let report = enrich_metadata(&s, &both_down, id).await.unwrap();
        assert!(
            matches!(report.outcome, EnrichOutcome::NoAnswer),
            "{:?}",
            report.outcome
        );
        assert_eq!(report.warnings.len(), 2);
        assert!(report.filled.is_empty());

        let half = vec![
            Fake::new(ProviderId::OpenLibrary).broken().boxed(),
            Fake::new(ProviderId::GoogleBooks).boxed(),
        ];
        let report = enrich_metadata(&s, &half, id).await.unwrap();
        assert!(matches!(report.outcome, EnrichOutcome::NoAnswer));

        let s2 = Storage::connect("sqlite::memory:").await.unwrap();
        let id2 = s2
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    isbn_13: Some("9781900000178".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let providers = vec![Fake::new(ProviderId::OpenLibrary).broken().boxed()];
        let report = enrich_metadata(&s2, &providers, id2).await.unwrap();
        assert!(matches!(report.outcome, EnrichOutcome::NoAnswer));
    }

    #[tokio::test]
    async fn a_book_with_no_isbn_and_no_title_is_unaskable() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s.upsert_book(&Book::default(), None).await.unwrap();
        let providers = vec![
            Fake::new(ProviderId::OpenLibrary)
                .finds(vec![record("Anything At All", &[])])
                .boxed(),
        ];
        let report = enrich_metadata(&s, &providers, id).await.unwrap();
        assert!(matches!(report.outcome, EnrichOutcome::Unaskable));
        assert!(report.filled.is_empty());
    }

    #[tokio::test]
    async fn a_user_owned_field_is_held_back_and_the_report_says_what_was_offered() {
        let (s, id) = sidecar_seeded("Middlemarch", &["George Eliot"]).await;
        s.enrich_book(
            id,
            &Book {
                page_count: Some(496),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();

        let providers = vec![
            Fake::new(ProviderId::OpenLibrary)
                .finds(vec![Book {
                    page_count: Some(490),
                    description: Some("a sweeping saga".into()),
                    ..record("Middlemarch", &["George Eliot"])
                }])
                .boxed(),
        ];
        let report = enrich_metadata(&s, &providers, id).await.unwrap();

        let book = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(book.page_count, Some(496), "the correction survives");
        assert_eq!(
            s.field_source(id, "page_count").await.unwrap().as_deref(),
            Some("user"),
            "and the provider does not take the claim either"
        );
        assert_eq!(book.description.as_deref(), Some("a sweeping saga"));

        assert_eq!(report.held.len(), 1, "{:?}", report.held);
        let held = &report.held[0];
        assert_eq!(held.field, "page_count");
        assert_eq!(held.offered, "490");
        assert_eq!(held.kept.as_deref(), Some("496"));
        assert_eq!(held.offered_by, Some(Source::OpenLibrary));
        assert!(
            !report.filled.iter().any(|c| c.field == "page_count"),
            "a held field is not also reported as filled"
        );
    }

    #[tokio::test]
    async fn an_index_held_because_the_user_owns_the_series_is_reported() {
        let (s, id) = sidecar_seeded("Dune Messiah", &["Frank Herbert"]).await;
        s.enrich_book(
            id,
            &Book {
                series: Some("Dune".into()),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();

        let providers = vec![
            Fake::new(ProviderId::OpenLibrary)
                .finds(vec![Book {
                    series: Some("Dune Chronicles".into()),
                    series_index: Some(2.0),
                    ..record("Dune Messiah", &["Frank Herbert"])
                }])
                .boxed(),
        ];
        let report = enrich_metadata(&s, &providers, id).await.unwrap();

        let book = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(book.series.as_deref(), Some("Dune"));
        assert_eq!(book.series_index, None);

        let mut held: Vec<&str> = report.held.iter().map(|h| h.field).collect();
        held.sort_unstable();
        assert_eq!(held, vec!["series", "series_index"], "{:?}", report.held);
        let index = report
            .held
            .iter()
            .find(|h| h.field == "series_index")
            .unwrap();
        assert_eq!(index.offered, "#2");
        assert_eq!(
            index.kept, None,
            "nothing was kept — the field is empty and stays empty"
        );
        assert!(
            !report.filled.iter().any(|c| c.field == "series_index"),
            "a held field is not also reported as filled"
        );
    }

    #[tokio::test]
    async fn agreeing_with_the_user_is_not_a_held_field() {
        let (s, id) = sidecar_seeded("Middlemarch", &["George Eliot"]).await;
        s.enrich_book(
            id,
            &Book {
                page_count: Some(490),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();
        let providers = vec![
            Fake::new(ProviderId::OpenLibrary)
                .finds(vec![Book {
                    page_count: Some(490),
                    ..record("Middlemarch", &["George Eliot"])
                }])
                .boxed(),
        ];
        let report = enrich_metadata(&s, &providers, id).await.unwrap();
        assert!(report.held.is_empty(), "{:?}", report.held);
    }

    #[tokio::test]
    async fn enriching_twice_reports_nothing_the_second_time() {
        let (s, id) = sidecar_seeded("Middlemarch", &["George Eliot"]).await;
        let make = || {
            vec![
                Fake::new(ProviderId::OpenLibrary)
                    .finds(vec![Book {
                        page_count: Some(490),
                        description: Some("a sweeping saga".into()),
                        ..record("Middlemarch", &["George Eliot"])
                    }])
                    .boxed(),
            ]
        };

        let first = enrich_metadata(&s, &make(), id).await.unwrap();
        assert!(!first.filled.is_empty());
        assert!(first.wrote_anything());

        let second = enrich_metadata(&s, &make(), id).await.unwrap();
        assert!(
            second.filled.is_empty(),
            "second run reported changes: {:?}",
            second.filled
        );
        assert!(!second.wrote_anything());
        assert!(second.held.is_empty());
    }

    #[tokio::test]
    async fn a_provider_that_fails_degrades_to_a_warning() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    isbn_13: Some("9781900000178".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let providers = vec![
            Fake::new(ProviderId::OpenLibrary).broken().boxed(),
            Fake::new(ProviderId::GoogleBooks)
                .isbn(Book {
                    page_count: Some(512),
                    ..record("Middlemarch", &["George Eliot"])
                })
                .boxed(),
        ];

        let report = enrich_metadata(&s, &providers, id).await.unwrap();
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].provider(), Some(ProviderId::OpenLibrary));
        assert_eq!(filled(&report, "page_count").as_deref(), Some("512"));
        assert_eq!(
            s.field_source(id, "page_count").await.unwrap().as_deref(),
            Some("googlebooks"),
            "the survivor is credited, not the one that fell over"
        );
    }

    #[tokio::test]
    async fn a_replaced_value_reports_what_it_replaced() {
        let (s, id) = sidecar_seeded("Middlemarch", &["George Eliot"]).await;
        s.enrich_book(
            id,
            &Book {
                publisher: Some("Unknown".into()),
                ..Default::default()
            },
            Some(Source::KOReader),
        )
        .await
        .unwrap();

        let providers = vec![
            Fake::new(ProviderId::OpenLibrary)
                .finds(vec![Book {
                    publisher: Some("Blackwood".into()),
                    ..record("Middlemarch", &["George Eliot"])
                }])
                .boxed(),
        ];
        let report = enrich_metadata(&s, &providers, id).await.unwrap();
        let change = report
            .filled
            .iter()
            .find(|c| c.field == "publisher")
            .expect("publisher changed");
        assert_eq!(change.before.as_deref(), Some("Unknown"));
        assert_eq!(change.after, "Blackwood");
        assert_eq!(change.source, Some(Source::OpenLibrary));
        assert_eq!(
            s.field_source(id, "publisher").await.unwrap().as_deref(),
            Some("openlibrary")
        );
    }

    #[tokio::test]
    async fn a_book_that_does_not_exist_is_an_error() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let providers: Vec<Box<dyn MetadataProvider>> = Vec::new();
        assert!(matches!(
            enrich_metadata(&s, &providers, 9999).await,
            Err(EngineError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn enriching_one_book_does_not_read_the_shelf() {
        let (s, id) = sidecar_seeded("Middlemarch", &["George Eliot"]).await;
        for n in 0..50 {
            s.upsert_book(&record(&format!("Decoy {n}"), &["Nobody"]), None)
                .await
                .unwrap();
        }
        let providers = vec![
            Fake::new(ProviderId::OpenLibrary)
                .finds(vec![Book {
                    page_count: Some(490),
                    ..record("Middlemarch", &["George Eliot"])
                }])
                .boxed(),
        ];

        let report = enrich_metadata(&s, &providers, id).await.unwrap();
        assert!(matches!(
            report.outcome,
            EnrichOutcome::Enriched(EnrichMatch::Title(_))
        ));
        assert_eq!(s.get_book(id).await.unwrap().unwrap().page_count, Some(490));
        for book in s
            .list_books(&crate::BookQuery::new(100, crate::storage::BookSort::Title))
            .await
            .unwrap()
        {
            if book
                .title
                .as_deref()
                .is_some_and(|t| t.starts_with("Decoy"))
            {
                assert_eq!(book.page_count, None, "{:?} was written to", book.title);
            }
        }
    }
}
