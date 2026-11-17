//! The free-text search: a `ParsedQuery` asked of every provider, merged and ranked.
//! At most two requests per provider: the plan, then a plain-text fallback or an author follow-up.

use std::collections::BTreeSet;

use super::query::{KnownNames, ParsedQuery, fold};
use super::{ask_provider, provider_deadline};
use crate::book::Book;
use crate::diagnostic::Diagnostic;
use crate::providers::{MetadataProvider, ProviderId, ProviderWork, SearchRequest};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Plan {
    pub primary: SearchRequest,
    pub fallback: Option<SearchRequest>,
    pub limit: u32,
}

pub(crate) fn plan(q: &ParsedQuery, limit: u32) -> Plan {
    let base = SearchRequest {
        limit,
        language: q.lang.clone(),
        ..Default::default()
    };
    let plain = || {
        (q.text.chars().count() >= 1).then(|| SearchRequest {
            query: Some(q.text.clone()),
            ..base.clone()
        })
    };
    if let Some(isbn) = &q.isbn {
        return Plan {
            primary: SearchRequest {
                isbn: Some(isbn.clone()),
                ..base.clone()
            },
            fallback: None,
            limit,
        };
    }
    let f = &q.fields;
    if f.title.is_some() || f.author.is_some() || f.series.is_some() {
        // Series has no field at either provider; it goes in the free text, where both search it.
        let free: Vec<&str> = [f.free.as_deref(), f.series.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let marked = q.marked().is_some();
        return Plan {
            primary: SearchRequest {
                title: f.title.clone(),
                author: f.author.clone(),
                query: (!free.is_empty()).then(|| free.join(" ")),
                ..base.clone()
            },
            // A marker can be part of a title (*Stand by Me*): if reading it as one found nothing,
            // ask the text as typed.
            fallback: if marked { plain() } else { None },
            limit,
        };
    }
    Plan {
        primary: SearchRequest {
            query: f.free.clone().or_else(|| Some(q.text.clone())),
            ..base
        },
        fallback: None,
        limit,
    }
}

pub(crate) struct ResultNames {
    authors: Vec<Vec<String>>,
}

impl ResultNames {
    pub(crate) fn of<'a>(books: impl IntoIterator<Item = &'a Book>) -> Self {
        Self::named(
            books
                .into_iter()
                .flat_map(|b| b.authors.iter().map(String::as_str)),
        )
    }

    pub(crate) fn named<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut seen: BTreeSet<Vec<String>> = BTreeSet::new();
        for a in names {
            let tokens: Vec<String> = fold(a).split_whitespace().map(str::to_string).collect();
            if !tokens.is_empty() {
                seen.insert(tokens);
            }
        }
        ResultNames {
            authors: seen.into_iter().collect(),
        }
    }

    pub(crate) fn and(mut self, more: &ResultNames) -> Self {
        let mut all: BTreeSet<Vec<String>> = self.authors.into_iter().collect();
        all.extend(more.authors.iter().cloned());
        self.authors = all.into_iter().collect();
        self
    }

    /// Every word of `folded` is in the name, and every name word longer than an initial is in it.
    fn whole(&self, folded: &str) -> bool {
        let want: Vec<&str> = folded.split_whitespace().collect();
        !want.is_empty()
            && self.authors.iter().any(|a| {
                covers(&want, a) && a.iter().all(|t| t.len() < 2 || want.contains(&t.as_str()))
            })
    }
}

/// Every word of `want` is a word or initial of `name`, and at least one has three letters or more.
fn covers(want: &[&str], name: &[String]) -> bool {
    want.iter().any(|w| w.chars().count() >= 3)
        && want.iter().all(|w| {
            name.iter()
                .any(|t| t == w || (w.chars().count() == 1 && t.starts_with(w)))
        })
}

impl KnownNames for ResultNames {
    fn knows_author(&self, folded: &str) -> bool {
        let want: Vec<&str> = folded.split_whitespace().collect();
        self.authors.iter().any(|a| covers(&want, a))
    }

    fn knows_whole_author(&self, folded: &str) -> bool {
        self.whole(folded)
    }
}

const AUTHOR_ROWS_ENOUGH: usize = 5;

fn author_follow_up(q: &ParsedQuery, works: &[ProviderWork], limit: u32) -> Option<SearchRequest> {
    if q.isbn.is_some() || q.fields != Default::default() || q.words.len() < 2 {
        return None;
    }
    let names = ResultNames::of(works.iter().map(|w| &w.found.book));
    if !names.whole(&q.folded) {
        return None;
    }
    let want: Vec<&str> = q.folded.split_whitespace().collect();
    let theirs = works
        .iter()
        .filter(|w| {
            w.found.book.authors.iter().any(|a| {
                let t: Vec<String> = fold(a).split_whitespace().map(str::to_string).collect();
                covers(&want, &t)
            })
        })
        .count();
    (theirs < AUTHOR_ROWS_ENOUGH).then(|| SearchRequest {
        author: Some(q.text.clone()),
        limit,
        language: q.lang.clone(),
        ..Default::default()
    })
}

pub(crate) async fn ask(
    p: &dyn MetadataProvider,
    q: &ParsedQuery,
    plan: &Plan,
) -> std::result::Result<Vec<ProviderWork>, Diagnostic> {
    let id = p.id();
    let deadline = provider_deadline();
    let mut works = ask_provider(p, deadline, p.search_works(&plan.primary)).await?;
    if works.is_empty()
        && let Some(fallback) = &plan.fallback
    {
        works = ask_provider(p, deadline, p.search_works(fallback)).await?;
    }
    if let Some(follow) = author_follow_up(q, &works, plan.limit) {
        match ask_provider(p, deadline, p.search_works(&follow)).await {
            Ok(more) => {
                let offset = works.len();
                works.extend(more.into_iter().map(|mut w| {
                    w.found.position += offset;
                    w.facts.found_by_author = Some(q.folded.clone());
                    w
                }));
            }
            Err(d) => tracing::debug!(provider = %id, kind = ?d.kind,
                "author follow-up failed; the first answer stands"),
        }
    }
    Ok(works)
}

/// Logged without detail: a provider's error text can hold the request URL, which holds the query.
pub(crate) fn log_degraded(id: ProviderId, d: &Diagnostic) {
    use crate::diagnostic::DiagnosticKind as K;
    let (kind, class) = match &d.kind {
        K::ProviderFailed { class, .. } => ("failed", Some(*class)),
        K::ProviderTimedOut { .. } => ("timed out", None),
        K::ProviderRateLimited { .. } => ("rate limited", None),
        _ => ("other", None),
    };
    tracing::warn!(provider = %id, kind, class = ?class, "provider degraded");
    tracing::trace!(provider = %id, detail = %d.detail, "provider degraded (detail)");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::providers::Manners;
    use crate::search::query::{KnowsNothing, parse};
    use crate::search::{PROVIDER_TIMEOUT, RankedResult};

    fn p(text: &str) -> Plan {
        plan(&parse(text, &KnowsNothing), 20)
    }

    #[test]
    fn the_plan_sends_what_was_marked_and_falls_back_to_the_text() {
        let pl = p("dune by frank herbert");
        assert_eq!(pl.primary.title.as_deref(), Some("dune"));
        assert_eq!(pl.primary.author.as_deref(), Some("frank herbert"));
        assert_eq!(pl.primary.query, None);
        assert_eq!(
            pl.fallback.and_then(|f| f.query).as_deref(),
            Some("dune by frank herbert")
        );

        let pl = p("Herbert, Dune");
        assert_eq!(
            pl.primary.title.as_deref(),
            Some("Dune"),
            "as typed, not folded"
        );
        assert_eq!(pl.primary.author.as_deref(), Some("Herbert"));
    }

    #[test]
    fn the_plan_for_plain_text_is_the_text() {
        let pl = p("dune herbert");
        assert_eq!(pl.primary.query.as_deref(), Some("dune herbert"));
        assert_eq!(pl.primary.title, None);
        assert_eq!(pl.fallback, None);
        assert_eq!(pl.limit, 20);
    }

    #[test]
    fn the_plan_for_an_isbn_is_the_isbn() {
        let pl = p("978-0-441-17271-9");
        assert_eq!(pl.primary.isbn.as_deref(), Some("9780441172719"));
        assert_eq!(pl.primary.query, None);
    }

    #[test]
    fn the_plan_for_fielded_text_is_its_fields() {
        let pl = p("title:Dune author:Herbert lang:en");
        assert_eq!(pl.primary.title.as_deref(), Some("Dune"));
        assert_eq!(pl.primary.author.as_deref(), Some("Herbert"));
        assert_eq!(pl.primary.language.as_deref(), Some("en"));
        assert_eq!(pl.fallback, None);
    }

    fn by(author: &str) -> Book {
        Book {
            title: Some("x".into()),
            authors: vec![author.into()],
            ..Default::default()
        }
    }

    #[test]
    fn names_known_from_results_are_whole_or_by_surname() {
        let names = ResultNames::of(&[by("Ursula K. Le Guin"), by("Frank Herbert")]);
        assert!(names.knows_author("herbert"));
        assert!(names.knows_author("frank herbert"));
        assert!(!names.knows_author("dune"));
        assert!(!names.knows_author("k"), "an initial alone is nobody");
        assert!(names.whole("ursula le guin"));
        assert!(names.whole("ursula k le guin"));
        assert!(!names.whole("herbert"), "a surname is not a whole name");
        assert!(!names.whole("dune herbert"));
    }

    use crate::providers::ProviderBook;
    use async_trait::async_trait;
    use std::sync::Mutex;

    type Script = Box<dyn Fn(&SearchRequest) -> Vec<(Book, u32)> + Send + Sync>;

    struct Scripted {
        answer: Script,
        asked: Mutex<Vec<SearchRequest>>,
        manners: Option<Manners>,
        hang: bool,
    }

    impl Scripted {
        fn new(
            answer: impl Fn(&SearchRequest) -> Vec<(Book, u32)> + Send + Sync + 'static,
        ) -> Self {
            Scripted {
                answer: Box::new(answer),
                asked: Mutex::new(Vec::new()),
                manners: None,
                hang: false,
            }
        }
        fn asked(&self) -> Vec<SearchRequest> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl MetadataProvider for Scripted {
        fn id(&self) -> ProviderId {
            ProviderId::OpenLibrary
        }
        async fn search(&self, req: &SearchRequest) -> Result<Vec<ProviderBook>> {
            Ok(self
                .search_works(req)
                .await?
                .into_iter()
                .map(|w| w.found)
                .collect())
        }
        async fn search_works(&self, req: &SearchRequest) -> Result<Vec<ProviderWork>> {
            self.asked.lock().unwrap().push(req.clone());
            if self.hang {
                tokio::time::sleep(PROVIDER_TIMEOUT * 2).await;
            }
            Ok((self.answer)(req)
                .into_iter()
                .enumerate()
                .map(|(position, (book, editions))| ProviderWork {
                    found: ProviderBook {
                        book,
                        provider: ProviderId::OpenLibrary,
                        position,
                    },
                    facts: crate::providers::WorkFacts {
                        edition_count: Some(editions),
                        ..Default::default()
                    },
                    kept: Vec::new(),
                })
                .collect())
        }
        async fn by_isbn(&self, _: &str) -> Result<Option<ProviderBook>> {
            if self.hang {
                tokio::time::sleep(PROVIDER_TIMEOUT * 2).await;
            }
            Ok(None)
        }
        fn manners(&self) -> Option<&Manners> {
            self.manners.as_ref()
        }
    }

    fn titled(title: &str, author: &str) -> Book {
        Book {
            title: Some(title.into()),
            authors: vec![author.into()],
            ..Default::default()
        }
    }

    async fn run(p: &std::sync::Arc<Scripted>, text: &str) -> (Vec<RankedResult>, Vec<Diagnostic>) {
        struct Shared(std::sync::Arc<Scripted>);
        #[async_trait]
        impl MetadataProvider for Shared {
            fn id(&self) -> ProviderId {
                self.0.id()
            }
            async fn search(&self, req: &SearchRequest) -> Result<Vec<ProviderBook>> {
                self.0.search(req).await
            }
            async fn search_works(&self, req: &SearchRequest) -> Result<Vec<ProviderWork>> {
                self.0.search_works(req).await
            }
            async fn by_isbn(&self, isbn: &str) -> Result<Option<ProviderBook>> {
                self.0.by_isbn(isbn).await
            }
            fn manners(&self) -> Option<&Manners> {
                self.0.manners()
            }
        }
        // No database: these run under a paused clock, which sqlx's pool cannot.
        let providers: Vec<Box<dyn MetadataProvider>> = vec![Box::new(Shared(p.clone()))];
        let roster = super::super::session::Roster {
            providers: std::sync::Arc::new(providers),
            availability: vec![(
                ProviderId::OpenLibrary,
                crate::providers::ProviderAvailability::Ready,
            )],
        };
        let done = super::super::session::SearchSession::start(
            "t".into(),
            text,
            super::super::session::SearchScope::Everything,
            20,
            roster,
            None,
            super::super::session::Mode::ToDone,
        )
        .await
        .unwrap()
        .finish()
        .await;
        let results = done
            .works
            .into_iter()
            .map(|row| RankedResult {
                book: row.work.flattened().clone(),
                sources: row.work.sources.clone(),
                score: row.work.score,
                shelf_id: None,
                claims: row.work.flattened_claims(),
            })
            .collect();
        (results, done.warnings)
    }

    #[tokio::test]
    async fn a_marked_query_that_finds_nothing_falls_back_to_the_text() {
        let p = std::sync::Arc::new(Scripted::new(|req| {
            if req.query.is_some() {
                vec![(titled("Stand by Me", "Ben E. King"), 3)]
            } else {
                Vec::new()
            }
        }));
        let (results, warnings) = run(&p, "stand by me").await;
        assert!(warnings.is_empty());
        assert_eq!(results.len(), 1);
        let asked = p.asked();
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert_eq!(asked[0].author.as_deref(), Some("me"));
        assert_eq!(asked[1].query.as_deref(), Some("stand by me"));
    }

    #[tokio::test]
    async fn an_author_s_name_asks_for_her_books_once() {
        let p = std::sync::Arc::new(Scripted::new(|req| match &req.author {
            Some(_) => vec![
                (titled("Norwegian Wood", "Haruki Murakami"), 56),
                (titled("Kafka on the Shore", "Haruki Murakami"), 61),
                (titled("1Q84", "Haruki Murakami"), 42),
                (titled("After Dark", "Haruki Murakami"), 23),
            ],
            None => vec![
                (titled("Haruki Murakami", "Jay Rubin"), 3),
                (titled("Haruki Murakami", "Matthew Strecher"), 2),
                (
                    titled("Haruki Murakami and the Music of Words", "Jay Rubin"),
                    7,
                ),
                (titled("Norwegian Wood", "Haruki Murakami"), 56),
            ],
        }));
        let (results, _) = run(&p, "haruki murakami").await;
        let asked = p.asked();
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert_eq!(asked[1].author.as_deref(), Some("haruki murakami"));
        let top4: Vec<&str> = results
            .iter()
            .take(4)
            .map(|r| r.book.authors[0].as_str())
            .collect();
        assert!(top4.iter().all(|a| *a == "Haruki Murakami"), "{top4:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_provider_is_not_asked_and_is_reported() {
        let mut s = Scripted::new(|_| vec![(titled("Dune", "Frank Herbert"), 100)]);
        let m = Manners::unlimited();
        m.close_for(std::time::Duration::from_secs(90));
        s.manners = Some(m);
        let p = std::sync::Arc::new(s);
        let (results, warnings) = run(&p, "dune").await;
        assert!(results.is_empty());
        assert!(p.asked().is_empty(), "a closed provider was asked");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].is_rate_limited());
        assert_eq!(
            warnings[0].to_string(),
            "openlibrary: asked to wait; not asked again for 90s"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_hanging_provider_times_out_once() {
        let mut s = Scripted::new(|_| Vec::new());
        s.hang = true;
        let p = std::sync::Arc::new(s);
        let start = tokio::time::Instant::now();
        let (results, warnings) = run(&p, "dune by herbert").await;
        assert!(results.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].is_timeout());
        assert_eq!(start.elapsed(), PROVIDER_TIMEOUT);
    }

    #[tokio::test(start_paused = true)]
    async fn an_isbn_lookup_asks_every_provider_at_once() {
        let mut a = Scripted::new(|_| Vec::new());
        a.hang = true;
        let mut b = Scripted::new(|_| Vec::new());
        b.hang = true;
        let providers: Vec<Box<dyn MetadataProvider>> = vec![Box::new(a), Box::new(b)];
        let start = tokio::time::Instant::now();
        let (found, warnings) = super::super::lookup_isbn_all(&providers, "9780441172719").await;
        assert!(found.is_empty());
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().all(Diagnostic::is_timeout));
        assert_eq!(
            start.elapsed(),
            PROVIDER_TIMEOUT,
            "asked one after the other"
        );
    }
}
