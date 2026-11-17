use std::sync::Arc;

use async_trait::async_trait;
use futures::future::try_join_all;
use reqwest::Client;
use serde::Deserialize;
use url::Url;

use super::{
    KeptRecord, Manners, MetadataProvider, ProviderBook, ProviderId, ProviderWork, RecordForm,
    RecordKind, SearchRequest, WorkFacts, normalize_language, normalize_subjects, retry_after,
    to_marc_language, year_of_date,
};
use crate::book::{Book, normalize_isbn};
use crate::error::{EngineError, Result};

/// The live host. Overridable only so tests can point at a local server — see
/// [`OpenLibraryProvider::with_base`].
const DEFAULT_BASE: &str = "https://openlibrary.org";

pub struct OpenLibraryProvider {
    client: Client,
    /// Scheme and host every request is built on, with no trailing slash.
    base: String,
    /// One request a second (Open Library's published anonymous rate), shared
    /// by every request this provider
    /// makes, and the *closed until* a `Retry-After` set. An `Arc` so the
    /// engine can keep one across rebuilds of the provider list.
    manners: Arc<Manners>,
}

impl OpenLibraryProvider {
    pub fn new(client: Client) -> Self {
        Self::with_base(client, DEFAULT_BASE)
    }

    /// Point every request this provider makes — the search, the edition
    /// lookup, its author and work fetches — at a different host.
    ///
    /// `GoogleBooksProvider::with_base`'s twin, and for the same reason: the
    /// search benchmark (`tests/search_bench.rs`) replays recorded responses
    /// over loopback through the real URL building and parsing. Cover URLs are
    /// not requests and stay on `covers.openlibrary.org`.
    pub fn with_base(client: Client, base: impl Into<String>) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        OpenLibraryProvider {
            client,
            base,
            manners: Arc::new(Manners::open_library()),
        }
    }

    /// Share a rate limit with other instances, or lift it.
    ///
    /// The engine passes one `Manners` to every provider list it builds, so a
    /// Google key change (which rebuilds the list) does not forget that Open
    /// Library asked us to wait. The search benchmark passes
    /// [`Manners::unlimited`], since its server is a loopback replay with no
    /// rate to keep.
    pub fn with_manners(mut self, manners: Arc<Manners>) -> Self {
        self.manners = manners;
        self
    }

    /// One GET, in turn, and refused without asking while the provider is
    /// closed.
    ///
    /// **Never carries the URL in an error.** A search URL holds the query and
    /// an edition URL the ISBN typed, both the user's private reading
    /// (never logged above `trace!`); reqwest puts the whole URL in its error text,
    /// and that text becomes a `Diagnostic`'s detail and a log field.
    async fn get(&self, url: &str) -> Result<reqwest::Response> {
        if self.manners.closed_for().is_some() {
            return Err(EngineError::Provider {
                provider: ProviderId::OpenLibrary,
                message: "asked to wait; not asked again yet".into(),
            });
        }
        self.manners.wait_turn().await;
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
        {
            if let Some(after) = retry_after(resp.headers()) {
                self.manners.close_for(after);
            }
            return Err(EngineError::Provider {
                provider: ProviderId::OpenLibrary,
                message: format!("HTTP {status}"),
            });
        }
        Ok(resp)
    }

    /// [`Self::get`], then the body of a success.
    async fn get_text(&self, url: &str) -> Result<String> {
        let resp = self
            .get(url)
            .await?
            .error_for_status()
            .map_err(reqwest::Error::without_url)?;
        Ok(resp.text().await.map_err(reqwest::Error::without_url)?)
    }

    /// The URL of a key Open Library hands back (`/authors/OL1A`,
    /// `/works/OL1W`). The key carries its own leading slash, so joining it with
    /// another one made `https://openlibrary.org//authors/…`, which Open
    /// Library answered anyway.
    fn key_url(&self, key: &str) -> String {
        format!("{}/{}.json", self.base, key.trim_start_matches('/'))
    }
}

// ---- /search.json ----------------------------------------------------------

#[cfg(test)]
#[derive(Deserialize, Debug)]
struct SearchResp {
    docs: Option<Vec<Works>>,
}

#[derive(Deserialize, Debug, Default)]
struct Works {
    title: Option<String>,
    subtitle: Option<String>,
    author_name: Option<Vec<String>>,
    first_publish_year: Option<i64>,
    cover_edition_key: Option<String>,
    key: Option<String>,
    language: Option<Vec<String>>,
    first_sentence: Option<Vec<String>>,
    number_of_pages_median: Option<i64>,
    edition_count: Option<u32>,
    series_name: Option<Vec<String>>,
    /// Strings in every response seen (`["2"]`), read as JSON values so a
    /// number would not fail the whole page.
    series_position: Option<Vec<serde_json::Value>>,
    /// The edition Open Library judged the best match for the query, nested
    /// under its work (`fields=editions,editions.…`).
    editions: Option<NestedEditions>,
}

#[derive(Deserialize, Debug, Default)]
struct NestedEditions {
    docs: Option<Vec<NestedEdition>>,
}

#[derive(Deserialize, Debug, Default)]
struct NestedEdition {
    title: Option<String>,
    subtitle: Option<String>,
    isbn: Option<Vec<String>>,
    language: Option<Vec<String>>,
    publisher: Option<Vec<String>>,
}

impl Works {
    /// `-L`, not `-M`.
    ///
    /// OpenLibrary publishes three sizes off one key: `-S` (~40px), `-M`
    /// (~180px) and `-L` (whatever the scan actually is, usually 500px+). `-M`
    /// was picked when the only consumer was a terminal drawing a book in
    /// Unicode half-blocks, where 180px is already more than the framebuffer
    /// can show. A cover-forward detail page on a 2× display is not that
    /// consumer, and there is no fourth size to ask for.
    ///
    /// The shelf does not pay for it: `images::store_cover` writes a downscaled
    /// tier beside anything over `THUMB_MAX` and `Book::shelf_cover_path` is
    /// what a grid loads. Asking for the large size *without* that tier would
    /// make every list strictly heavier than it was, which is why the two
    /// halves of 20c are one change and not two.
    fn cover_url(&self) -> Option<String> {
        self.cover_edition_key
            .as_deref()
            .map(|k| format!("https://covers.openlibrary.org/b/olid/{k}-L.jpg"))
    }

    fn best_edition(&self) -> Option<&NestedEdition> {
        self.editions.as_ref()?.docs.as_ref()?.first()
    }

    /// The book a result row stands for: the work's title, authors and year,
    /// with the **best matching edition's** ISBNs, language and publisher.
    ///
    /// The ISBNs used to be the first valid ones in the work's `isbn` list,
    /// which is every edition's in no stated order — an arbitrary edition, and a
    /// list long enough (hundreds, for a famous book) to be most of a response.
    /// The nested edition is the one Open Library matched to what was typed, so
    /// its ISBN, language and publisher at least describe one edition together.
    fn to_book(&self) -> Book {
        let edition = self.best_edition();
        // Scan the isbn list for the first valid 10 and first valid 13.
        let mut isbn_10 = None;
        let mut isbn_13 = None;
        for raw in edition.and_then(|e| e.isbn.as_ref()).into_iter().flatten() {
            if let Some(norm) = normalize_isbn(raw) {
                match norm.len() {
                    10 if isbn_10.is_none() => isbn_10 = Some(norm),
                    13 if isbn_13.is_none() => isbn_13 = Some(norm),
                    _ => {}
                }
            }
            if isbn_10.is_some() && isbn_13.is_some() {
                break;
            }
        }
        Book {
            title: self.title.clone(),
            authors: self.author_name.clone().unwrap_or_default(),
            publish_year: self.first_publish_year,
            first_published: self.first_publish_year,
            language: edition
                .and_then(|e| e.language.as_ref())
                .or(self.language.as_ref())
                .and_then(|v| v.first())
                .map(|l| normalize_language(l)),
            isbn_10,
            isbn_13,
            openlibrary_key: self.key.clone(),
            publisher: edition
                .and_then(|e| e.publisher.as_ref())
                .and_then(|v| v.first().cloned()),
            page_count: self.number_of_pages_median,
            first_sentence: self
                .first_sentence
                .as_ref()
                .and_then(|v| v.first().cloned()),
            cover_url: self.cover_url(),
            ..Default::default()
        }
    }

    fn facts(&self) -> WorkFacts {
        let edition = self.best_edition();
        let mut series = self
            .series_name
            .as_ref()
            .and_then(|v| v.first())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let mut series_position = series.as_ref().and_then(|_| {
            let v = self.series_position.as_ref()?.first()?;
            let n = match v {
                serde_json::Value::Number(n) => n.as_f64(),
                serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
                _ => None,
            }?;
            n.is_finite().then_some(n)
        });
        // Open Library's series records are sparse; a matched edition's title
        // often says it instead (`The Well of Ascension (Mistborn, Book 2)`).
        // Read only for ranking, never written to a book.
        if series.is_none()
            && let Some((name, position)) = edition
                .and_then(|e| e.title.as_deref())
                .and_then(series_in_parentheses)
        {
            series = Some(name);
            series_position = Some(position);
        }
        let mut languages: Vec<String> = Vec::new();
        for l in self.language.iter().flatten() {
            let l = normalize_language(l);
            if !languages.contains(&l) {
                languages.push(l);
            }
        }
        let mut also_titled: Vec<String> = Vec::new();
        let mut also = |title: Option<&String>, subtitle: Option<&String>| {
            let Some(t) = title.map(|t| t.trim()).filter(|t| !t.is_empty()) else {
                return;
            };
            let full = match subtitle.map(|s| s.trim()).filter(|s| !s.is_empty()) {
                Some(sub) => format!("{t}: {sub}"),
                None => t.to_string(),
            };
            if !also_titled.contains(&full) {
                also_titled.push(full);
            }
        };
        if self.subtitle.is_some() {
            also(self.title.as_ref(), self.subtitle.as_ref());
        }
        if let Some(e) = edition {
            also(e.title.as_ref(), e.subtitle.as_ref());
        }
        WorkFacts {
            edition_count: self.edition_count,
            also_titled,
            series,
            series_position,
            languages,
            found_by_author: None,
        }
    }
}

/// `(Mistborn, Book 2)`, `(Dune Chronicles #3)`, `(Legends of Dune, Vol. 2)`
/// at the end of an edition title: the series and the place. Anything else,
/// `None` — `(National Book Award Finalist)` names no series.
fn series_in_parentheses(title: &str) -> Option<(String, f64)> {
    let inner = title.trim().strip_suffix(')')?;
    let inner = &inner[inner.rfind('(')? + 1..];
    let lower = inner.to_ascii_lowercase();
    for marker in [", book ", " book ", ", vol. ", ", volume ", ", #", " #"] {
        if let Some(at) = lower.rfind(marker) {
            let name = inner[..at].trim().trim_end_matches(',').trim();
            let n = inner[at + marker.len()..].trim().parse::<f64>().ok()?;
            if name.is_empty() || !n.is_finite() || n <= 0.0 {
                return None;
            }
            return Some((name.to_string(), n));
        }
    }
    None
}

/// `search.json`'s answer with each doc left as JSON, for [`work_of_doc`].
#[derive(Deserialize, Debug)]
struct RawSearchResp {
    docs: Option<Vec<serde_json::Value>>,
}

/// One `search.json` doc as a result, with the records it may be kept as: the
/// work (the doc less its `editions`) and the edition Open Library matched
/// (`editions.docs[0]`), each keyed by its own Open Library key. A doc with no
/// work key cannot be kept — there would be nothing to find it by — and is
/// still a result.
fn work_of_doc(doc: serde_json::Value, position: usize) -> Result<ProviderWork> {
    let works: Works = serde_json::from_value(doc.clone())?;
    let mut kept = Vec::new();
    if let (Some(work_key), serde_json::Value::Object(mut map)) = (works.key.clone(), doc) {
        let edition = map
            .remove("editions")
            .and_then(|mut e| e.get_mut("docs")?.as_array_mut()?.drain(..).next());
        kept.push(KeptRecord {
            key: work_key,
            kind: RecordKind::Work,
            form: RecordForm::Doc,
            title: works.title.clone(),
            authors: works.author_name.clone().unwrap_or_default(),
            body: serde_json::Value::Object(map).to_string(),
        });
        if let Some(edition) = edition
            && let Some(key) = edition
                .get("key")
                .and_then(|k| k.as_str())
                .map(str::to_string)
        {
            kept.push(KeptRecord {
                key,
                kind: RecordKind::Edition,
                form: RecordForm::Doc,
                title: edition
                    .get("title")
                    .and_then(|t| t.as_str())
                    .map(str::to_string),
                authors: Vec::new(),
                body: edition.to_string(),
            });
        }
    }
    Ok(ProviderWork {
        found: ProviderBook {
            book: works.to_book(),
            provider: ProviderId::OpenLibrary,
            position,
        },
        facts: works.facts(),
        kept,
    })
}

/// The inverse of [`work_of_doc`]: a kept work and its kept edition, read back
/// into the result they were. `Err` when either body no longer reads as what
/// Open Library sends, which is what makes a kept record *unreadable*: it is
/// dropped and asked for again.
pub(crate) fn decode_doc(
    work: &str,
    edition: Option<&str>,
    position: usize,
) -> Result<ProviderWork> {
    let mut doc: serde_json::Value = serde_json::from_str(work)?;
    let serde_json::Value::Object(map) = &mut doc else {
        return Err(EngineError::Provider {
            provider: ProviderId::OpenLibrary,
            message: "a kept work is not an object".into(),
        });
    };
    if let Some(edition) = edition {
        let edition: serde_json::Value = serde_json::from_str(edition)?;
        map.insert("editions".into(), serde_json::json!({ "docs": [edition] }));
    }
    let mut w = work_of_doc(doc, position)?;
    if w.found.book.title.is_none() {
        return Err(EngineError::Provider {
            provider: ProviderId::OpenLibrary,
            message: "a kept work has no title".into(),
        });
    }
    w.kept.clear();
    Ok(w)
}

/// What `search.json` is asked for.
///
/// **No `subject` facet**: it would enlarge every response of every search by
/// the noisiest form of the field, for data only a saved book has a use for
/// (subjects come off the work, in `by_isbn`). And **no work-level `isbn` or
/// `publisher` lists** either: both are every edition's,
/// unordered, and for a famous book they were most of the response. The nested
/// best edition (`editions.*`) carries the ISBN, language and publisher of one
/// edition instead, and `edition_count`/`series_*` feed the ranking.
const SEARCH_FIELDS: &str = "key,title,subtitle,author_name,language,first_sentence,\
     first_publish_year,cover_edition_key,number_of_pages_median,edition_count,series_key,\
     series_name,series_position,editions,editions.key,editions.title,editions.subtitle,\
     editions.isbn,editions.language,editions.publisher";

/// Open Library refuses a `q` under this many characters with a 422 (found
/// while recording the benchmark), so a shorter text is sent as a `title`.
const MIN_Q_CHARS: usize = 3;

/// Free text made safe to put in `q`, which Open Library parses as a Solr
/// query: a `:` would name a field (`sapiens: a brief history`), a bare `-`
/// negates the word after it (`the road - mccarthy` found nothing), and an
/// unbalanced quote or bracket is a syntax error. Each becomes a space.
fn plain_q(text: &str) -> String {
    let spaced: String = text
        .chars()
        .map(|c| match c {
            ':' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '~' | '*' | '?' | '\\' | '!'
            | '+' | '|' | '&' => ' ',
            c => c,
        })
        .collect();
    spaced
        .split_whitespace()
        .map(|w| w.trim_start_matches('-'))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn build_search_url(base: &str, req: &SearchRequest) -> Result<String> {
    let mut params: Vec<(&str, String)> = Vec::new();
    let mut q_parts: Vec<String> = Vec::new();

    let mut short_title: Option<String> = None;
    if let Some(q) = &req.query {
        let q = plain_q(q);
        if q.chars().count() < MIN_Q_CHARS {
            // Never a bare `q` under three characters: *It* and `三体` go as a
            // title, which Open Library takes at any length.
            if !q.is_empty() && req.title.is_none() {
                short_title = Some(q);
            }
        } else {
            q_parts.push(q);
        }
    }
    if let Some(t) = &req.translator {
        // No translator facet; contributor is the closest solr field.
        q_parts.push(format!("contributor:\"{t}\""));
    }
    if !q_parts.is_empty() {
        params.push(("q", q_parts.join(" ")));
    }
    if let Some(t) = req.title.as_ref().or(short_title.as_ref()) {
        params.push(("title", t.clone()));
    }
    if let Some(a) = &req.author {
        params.push(("author", a.clone()));
    }
    if let Some(p) = &req.publisher {
        params.push(("publisher", p.clone()));
    }
    if let Some(l) = &req.language {
        params.push(("lang", to_marc_language(l)));
    }
    if let Some(i) = &req.isbn {
        params.push(("q", format!("isbn:{i}")));
    }

    let limit = if req.limit == 0 { 30 } else { req.limit };
    let mut url = Url::parse_with_params(&format!("{base}/search.json"), &params)?;
    url.query_pairs_mut()
        .append_pair("limit", &limit.to_string())
        .append_pair("fields", SEARCH_FIELDS);
    Ok(url.into())
}

// ---- /isbn/{isbn}.json (edition lookup) ------------------------------------

#[derive(Deserialize, Debug)]
struct Key {
    key: Option<String>,
}

#[derive(Deserialize, Debug)]
struct AuthorJson {
    name: Option<String>,
}

#[derive(Deserialize, Debug)]
struct EditionJson {
    #[serde(alias = "author")]
    authors: Option<Vec<Key>>,
    title: Option<String>,
    isbn_10: Option<Vec<String>>,
    isbn_13: Option<Vec<String>>,
    publish_date: Option<String>,
    publishers: Option<Vec<String>>,
    number_of_pages: Option<i64>,
    pagination: Option<String>,
    key: Option<String>,
    languages: Option<Vec<Key>>,
    covers: Option<Vec<i64>>,
    /// Free text, one entry per series the *edition* belongs to:
    /// `["Dune Chronicles #2"]`, `["Penguin classics"]`. Only the first is
    /// read — a book in two series is a shelving question and this column is a
    /// scalar pair.
    series: Option<Vec<String>>,
    /// The work(s) this edition realises. Subjects live on the **work**, never
    /// on the edition, which is why they cost a second request.
    works: Option<Vec<Key>>,
}

/// The slice of `/works/{key}.json` this reads. Nothing else on a work is
/// wanted: title, description and covers are edition-level facts here and the
/// edition record already answered them.
#[derive(Deserialize, Debug)]
struct WorkJson {
    subjects: Option<Vec<String>>,
}

/// Split `"Dune Chronicles #2"` into a name and a position.
///
/// **Two separators and no more.** OpenLibrary's `series` is free text and
/// carries everything from `Dune Chronicles #2` to `Penguin classics` to
/// `Bd. 3 : Der Zauberberg`, and a parser that keeps guessing eventually reads
/// a number that is not an index — a wrong "#2" is a claim about *which book
/// this is*, and nothing downstream can tell it from a right one. So: a
/// trailing `#N` or `; N`, and otherwise the whole string is the name and the
/// index is absent. Absence is the honest answer and the pair guard means it
/// stays absent rather than being filled from somewhere else.
fn split_series(raw: &str) -> Option<(String, Option<f64>)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    for sep in ['#', ';'] {
        if let Some((name, tail)) = raw.rsplit_once(sep) {
            let name = name.trim_end_matches([',', ' ', ':']).trim();
            if let Ok(index) = tail.trim().parse::<f64>()
                && index.is_finite()
                && !name.is_empty()
            {
                return Some((name.to_string(), Some(index)));
            }
        }
    }
    Some((raw.to_string(), None))
}

async fn author_of_key(key: &Key, ol: &OpenLibraryProvider) -> Result<Option<String>> {
    let Some(k) = key.key.as_deref() else {
        return Ok(None);
    };
    let res = ol.get_text(&ol.key_url(k)).await?;
    let author: AuthorJson = serde_json::from_str(&res)?;
    Ok(author.name)
}

/// Subjects, from the work this edition realises.
///
/// **One extra request per book looked up**, and that is the cost of this
/// field: `by_isbn` was 1 + *authors* requests and is now 2 + *authors*.
/// OpenLibrary's edition record does not carry subjects at all — they are a
/// property of the *work* — so there is no cheaper form of the question. It is
/// issued concurrently with the author lookups, so it costs a request rather
/// than a round trip, and `search.json` deliberately does **not** ask for its
/// `subject` facet: that would enlarge every response of every search by the
/// noisiest form of this field for data only a saved book has any use for.
///
/// **A failure here is not a failure of the lookup.** The edition record has
/// already answered the question that was asked; subjects are additive. So this
/// degrades to none rather than propagating, which is the same rule the
/// federated search applies one level up — and unlike the author resolution
/// below, whose result is part of identity.
async fn subjects_of_work(key: &Key, ol: &OpenLibraryProvider) -> Vec<String> {
    let Some(k) = key.key.as_deref() else {
        return Vec::new();
    };
    let url = ol.key_url(k);
    let fetched = async {
        let text = ol.get_text(&url).await?;
        let work: WorkJson = serde_json::from_str(&text)?;
        Ok::<_, EngineError>(work.subjects.unwrap_or_default())
    }
    .await;
    match fetched {
        Ok(subjects) => normalize_subjects(subjects),
        Err(e) => {
            tracing::debug!(provider = %ProviderId::OpenLibrary, work = %k, detail = %e,
                "work subjects unavailable; the edition still answered");
            Vec::new()
        }
    }
}

async fn edition_to_book(edition: EditionJson, ol: &OpenLibraryProvider) -> Result<Book> {
    // Resolve author keys to names concurrently, and ask the work about its
    // subjects in the same flight.
    let keys = edition.authors.unwrap_or_default();
    let work = edition.works.as_ref().and_then(|w| w.first());
    let (authors, subjects) = futures::future::join(
        try_join_all(keys.iter().map(|k| author_of_key(k, ol))),
        async {
            match work {
                Some(w) => subjects_of_work(w, ol).await,
                None => Vec::new(),
            }
        },
    )
    .await;
    let authors: Vec<String> = authors?.into_iter().flatten().collect();

    let page_count = edition
        .number_of_pages
        .or_else(|| edition.pagination.as_deref().and_then(|p| p.parse().ok()));

    fn first_valid(opt: Option<Vec<String>>) -> Option<String> {
        opt?.iter().find_map(|v| normalize_isbn(v))
    }

    let language = edition
        .languages
        .as_ref()
        .and_then(|v| v.first())
        .and_then(|k| k.key.as_deref())
        .and_then(|k| k.rsplit('/').next().map(normalize_language));

    let (series, series_index) = edition
        .series
        .as_ref()
        .and_then(|v| v.first())
        .and_then(|s| split_series(s))
        .map_or((None, None), |(name, index)| (Some(name), index));

    Ok(Book {
        title: edition.title,
        authors,
        subjects,
        series,
        series_index,
        publish_year: edition.publish_date.as_deref().and_then(year_of_date),
        openlibrary_key: edition.key,
        page_count,
        publisher: edition.publishers.and_then(|v| v.into_iter().next()),
        language,
        cover_url: edition
            .covers
            .as_ref()
            .and_then(|v| v.first())
            .map(|id| format!("https://covers.openlibrary.org/b/id/{id}-M.jpg")),
        isbn_10: first_valid(edition.isbn_10),
        isbn_13: first_valid(edition.isbn_13),
        ..Default::default()
    })
}

#[async_trait]
impl MetadataProvider for OpenLibraryProvider {
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
        let url = build_search_url(&self.base, req)?;
        let text = self.get_text(&url).await?;
        // Each doc is read twice from one parse: once as `Works`, and once as
        // the raw JSON it was, split into the work and its matched edition so
        // both can be kept as Open Library sent them — split, so another query
        // matching another edition of the work cannot overwrite this one's.
        let resp: RawSearchResp = serde_json::from_str(&text)?;
        resp.docs
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(position, doc)| work_of_doc(doc, position))
            .collect()
    }

    fn manners(&self) -> Option<&Manners> {
        Some(&self.manners)
    }

    async fn by_isbn(&self, isbn: &str) -> Result<Option<ProviderBook>> {
        let url = format!("{}/isbn/{isbn}.json", self.base);
        let resp = self.get(&url).await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let text = resp
            .error_for_status()
            .map_err(reqwest::Error::without_url)?
            .text()
            .await
            .map_err(reqwest::Error::without_url)?;
        let edition: EditionJson =
            serde_json::from_str(&text).map_err(|e| EngineError::Provider {
                provider: ProviderId::OpenLibrary,
                message: format!("edition decode: {e}"),
            })?;
        let book = edition_to_book(edition, self).await?;
        Ok(Some(ProviderBook {
            book,
            provider: ProviderId::OpenLibrary,
            position: 0,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_url_maps_fields() {
        let req = SearchRequest {
            title: Some("The Trial".into()),
            author: Some("Kafka".into()),
            publisher: Some("Schocken".into()),
            translator: Some("Breon Mitchell".into()),
            language: Some("en".into()),
            limit: 10,
            ..Default::default()
        };
        let url = build_search_url(DEFAULT_BASE, &req).unwrap();
        assert!(url.contains("title=The+Trial"));
        assert!(url.contains("author=Kafka"));
        assert!(url.contains("publisher=Schocken"));
        assert!(url.contains("lang=eng"));
        assert!(url.contains("contributor"));
        assert!(url.contains("limit=10"));
    }

    #[test]
    fn works_doc_deserializes_and_converts() {
        let json = r#"{
            "docs": [{
                "title": "Middlemarch",
                "author_name": ["George Eliot"],
                "first_publish_year": 2017,
                "cover_edition_key": "OL9000001M",
                "key": "/works/OL9000001W",
                "language": ["fre", "eng"],
                "number_of_pages_median": 490,
                "edition_count": 44,
                "series_name": ["Some Series"],
                "series_position": ["2"],
                "editions": {"numFound": 40, "docs": [{
                    "key": "/books/OL9000001M",
                    "title": "Middlemarch (Illustrated Edition)",
                    "isbn": ["9781900000178", "1900000172", "bogus"],
                    "language": ["eng"],
                    "publisher": ["Blackwood"]
                }]}
            }]
        }"#;
        let resp: SearchResp = serde_json::from_str(json).unwrap();
        let w = &resp.docs.unwrap()[0];
        let b = w.to_book();
        assert_eq!(
            b.title.as_deref(),
            Some("Middlemarch"),
            "the work's title, not the edition's"
        );
        assert_eq!(b.isbn_13.as_deref(), Some("9781900000178"));
        assert_eq!(b.isbn_10.as_deref(), Some("1900000172"));
        assert_eq!(b.language.as_deref(), Some("en"), "the edition's language");
        assert_eq!(b.publisher.as_deref(), Some("Blackwood"));
        assert_eq!(b.page_count, Some(490));
        assert_eq!(b.publish_year, Some(2017));
        assert_eq!(b.first_published, Some(2017));
        assert_eq!(b.openlibrary_key.as_deref(), Some("/works/OL9000001W"));
        assert!(b.cover_url.unwrap().contains("OL9000001M"));

        let f = w.facts();
        assert_eq!(f.edition_count, Some(44));
        assert_eq!(f.also_titled, vec!["Middlemarch (Illustrated Edition)"]);
        assert_eq!(
            series_in_parentheses("Middlemarch (Illustrated Edition)"),
            None
        );
        assert_eq!(f.series.as_deref(), Some("Some Series"));
        assert_eq!(f.series_position, Some(2.0));
        assert_eq!(f.languages, vec!["fr", "en"]);
    }

    /// A doc is kept as two records — the work, and the edition Open Library
    /// matched — and the two read back into the same result: the same book,
    /// the same facts. The work's record carries no edition, so a later search
    /// matching a different edition cannot overwrite this one's.
    #[test]
    fn a_doc_is_kept_as_its_work_and_its_edition_and_reads_back_the_same() {
        let doc: serde_json::Value = serde_json::from_str(
            r#"{
                "title": "Middlemarch",
                "author_name": ["George Eliot"],
                "first_publish_year": 2017,
                "key": "/works/OL9000001W",
                "language": ["fre", "eng"],
                "edition_count": 44,
                "editions": {"numFound": 40, "docs": [{
                    "key": "/books/OL9000001M",
                    "title": "Middlemarch (Illustrated Edition)",
                    "isbn": ["9781900000178", "1900000172"],
                    "language": ["eng"],
                    "publisher": ["Blackwood"]
                }]}
            }"#,
        )
        .unwrap();
        let w = work_of_doc(doc, 3).unwrap();
        assert_eq!(w.kept.len(), 2);
        assert_eq!(w.kept[0].key, "/works/OL9000001W");
        assert_eq!(w.kept[0].kind, RecordKind::Work);
        assert!(!w.kept[0].body.contains("OL9000001M"), "{}", w.kept[0].body);
        assert_eq!(w.kept[1].key, "/books/OL9000001M");
        assert_eq!(w.kept[1].kind, RecordKind::Edition);

        let back = decode_doc(&w.kept[0].body, Some(&w.kept[1].body), 3).unwrap();
        assert_eq!(format!("{:?}", back.found), format!("{:?}", w.found));
        assert_eq!(back.facts, w.facts);
        assert!(back.kept.is_empty());

        // Without its edition the work still reads, with no ISBN.
        let bare = decode_doc(&w.kept[0].body, None, 0).unwrap();
        assert_eq!(bare.found.book.isbn_13, None);
        assert_eq!(bare.found.book.title.as_deref(), Some("Middlemarch"));
        // And a body that is not Open Library's is refused, not guessed at.
        assert!(decode_doc("[1,2]", None, 0).is_err());
        assert!(decode_doc("{\"key\": \"/works/X\"}", None, 0).is_err());
    }

    /// A work with no nested edition still converts, with no ISBN rather than
    /// some other edition's.
    #[test]
    fn a_work_without_a_matched_edition_has_no_isbn() {
        let json =
            r#"{"docs": [{"title": "Dune", "key": "/works/OL893414W", "language": ["eng"]}]}"#;
        let resp: SearchResp = serde_json::from_str(json).unwrap();
        let b = resp.docs.unwrap()[0].to_book();
        assert_eq!(b.isbn_13, None);
        assert_eq!(b.isbn_10, None);
        assert_eq!(
            b.language.as_deref(),
            Some("en"),
            "the work's, failing an edition's"
        );
    }

    /// Open Library 422s a `q` under three characters, and parses `q` as Solr.
    #[test]
    fn a_short_query_goes_as_a_title_and_q_is_never_solr_syntax() {
        let short = SearchRequest {
            query: Some("It".into()),
            ..Default::default()
        };
        let url = build_search_url(DEFAULT_BASE, &short).unwrap();
        assert!(url.contains("title=It"), "{url}");
        assert!(!url.contains("q="), "{url}");

        let cjk = SearchRequest {
            query: Some("三体".into()),
            ..Default::default()
        };
        let url = build_search_url(DEFAULT_BASE, &cjk).unwrap();
        assert!(!url.contains("q="), "{url}");

        let solr = SearchRequest {
            query: Some("the road - mccarthy: \"x".into()),
            ..Default::default()
        };
        let url = url::Url::parse(&build_search_url(DEFAULT_BASE, &solr).unwrap()).unwrap();
        let q: String = url
            .query_pairs()
            .find(|(k, _)| k == "q")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        assert_eq!(q, "the road mccarthy x");
        assert_eq!(plain_q("slaughterhouse-five"), "slaughterhouse-five");
    }

    /// A 429 with `Retry-After` closes the provider for that long: the next
    /// search is refused without a request, the error carries no URL (the URL
    /// is the query), and the search after the wait asks again. Loopback only.
    #[tokio::test]
    async fn retry_after_closes_the_provider_and_no_url_leaks() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
            .expect(1)
            .mount(&server)
            .await;
        let ol = OpenLibraryProvider::with_base(
            Client::builder().no_proxy().build().unwrap(),
            server.uri(),
        );
        let req = SearchRequest {
            query: Some("a private query".into()),
            ..Default::default()
        };
        let err = ol.search(&req).await.unwrap_err();
        let text = err.to_string();
        assert!(!text.contains("private"), "{text}");
        assert!(text.contains("429"), "{text}");
        let left = ol.manners().and_then(Manners::closed_for).expect("closed");
        assert!(left <= std::time::Duration::from_secs(1));

        // Closed: refused without a request (the mock expects exactly one).
        let again = ol.search(&req).await.unwrap_err().to_string();
        assert!(!again.contains("private"), "{again}");
        server.verify().await;
    }

    /// A 503 without `Retry-After` is a failure and closes nothing, and an
    /// ordinary HTTP error carries no URL either.
    #[tokio::test]
    async fn a_failure_without_retry_after_closes_nothing() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let ol = OpenLibraryProvider::with_base(
            Client::builder().no_proxy().build().unwrap(),
            server.uri(),
        );
        let req = SearchRequest {
            query: Some("a private query".into()),
            ..Default::default()
        };
        let text = ol.search(&req).await.unwrap_err().to_string();
        assert!(
            !text.contains("private") && !text.contains("search.json"),
            "{text}"
        );
        assert_eq!(ol.manners().and_then(Manners::closed_for), None);
        let text = ol.by_isbn("9780441172719").await.unwrap_err().to_string();
        assert!(!text.contains("9780441172719"), "{text}");
    }

    #[test]
    fn a_series_is_read_from_an_edition_title_only_when_it_is_plain() {
        let cases = [
            (
                "The Well of Ascension (Mistborn, Book 2)",
                Some(("Mistborn", 2.0)),
            ),
            (
                "Children of Dune (Dune Chronicles, Book 3)",
                Some(("Dune Chronicles", 3.0)),
            ),
            (
                "House Harkonnen (Dune: House Trilogy, Book 2)",
                Some(("Dune: House Trilogy", 2.0)),
            ),
            ("Dragonsong (Harper Hall #1)", Some(("Harper Hall", 1.0))),
            ("Middlemarch (Illustrated Edition)", None),
            ("Dune (40th Anniversary)", None),
            ("Words of Radiance", None),
            ("Catch-22 (Book 0)", None),
        ];
        for (title, want) in cases {
            let got = series_in_parentheses(title);
            assert_eq!(got, want.map(|(n, p)| (n.to_string(), p)), "{title}");
        }
    }

    #[test]
    fn a_key_joins_the_base_with_one_slash() {
        let ol = OpenLibraryProvider::with_base(Client::new(), "http://127.0.0.1:9/ol/");
        assert_eq!(
            ol.key_url("/authors/OL1A"),
            "http://127.0.0.1:9/ol/authors/OL1A.json"
        );
        assert_eq!(
            ol.key_url("works/OL1W"),
            "http://127.0.0.1:9/ol/works/OL1W.json"
        );
    }

    /// The two shapes that are read, and the many that are deliberately not.
    ///
    /// The `None` cases matter more than the `Some` ones: an index parsed out of
    /// `Bd. 3 : Der Zauberberg` would be a confident claim about which book this
    /// is, and the pair guard cannot help with a wrong value — only with a
    /// missing one.
    #[test]
    fn series_text_is_split_only_where_it_is_unambiguous() {
        let cases = [
            ("Dune Chronicles #2", Some(("Dune Chronicles", Some(2.0)))),
            ("Dune Chronicles, #2", Some(("Dune Chronicles", Some(2.0)))),
            ("Discworld ; 5", Some(("Discworld", Some(5.0)))),
            // Novellas are numbered like this, which is why the column is REAL.
            ("Wayfarers #1.5", Some(("Wayfarers", Some(1.5)))),
            ("  Penguin classics  ", Some(("Penguin classics", None))),
            // A number that is not an index, and a separator that is not one.
            (
                "Bd. 3 : Der Zauberberg",
                Some(("Bd. 3 : Der Zauberberg", None)),
            ),
            (
                "The Lord of the Rings, Part 1",
                Some(("The Lord of the Rings, Part 1", None)),
            ),
            ("#3", Some(("#3", None))),
            ("   ", None),
        ];
        for (raw, want) in cases {
            let got = split_series(raw);
            let want = want.map(|(n, i)| (n.to_string(), i));
            assert_eq!(got, want, "{raw:?}");
        }
    }

    /// An edition with no work and no authors reaches no network at all, which
    /// is what makes the series half testable under the no-network rule. The
    /// subjects half needs `/works/`, which only a server behind
    /// [`OpenLibraryProvider::with_base`] can answer.
    #[tokio::test]
    async fn an_edition_carries_its_series_and_no_subjects_of_its_own() {
        let json = r#"{
            "title": "Dune Messiah",
            "isbn_13": ["9780441013593"],
            "series": ["Dune Chronicles #2"]
        }"#;
        let edition: EditionJson = serde_json::from_str(json).unwrap();
        let ol = OpenLibraryProvider::new(Client::new());
        let book = edition_to_book(edition, &ol).await.unwrap();
        assert_eq!(book.series.as_deref(), Some("Dune Chronicles"));
        assert_eq!(book.series_index, Some(2.0));
        assert!(
            book.subjects.is_empty(),
            "subjects are a property of the work, never of the edition"
        );
    }

    #[test]
    fn edition_json_deserializes() {
        let json = r#"{
            "title": "Middlemarch",
            "authors": [{"key": "/authors/OL7247542A"}],
            "isbn_10": ["1900000172"],
            "isbn_13": ["9781900000178"],
            "publish_date": "Aug 25, 2017",
            "publishers": ["William Blackwood and Sons"],
            "number_of_pages": 490,
            "key": "/books/OL9000001M",
            "languages": [{"key": "/languages/eng"}],
            "covers": [8309121],
            "works": [{"key": "/works/OL9000001W"}]
        }"#;
        let e: EditionJson = serde_json::from_str(json).unwrap();
        assert_eq!(e.number_of_pages, Some(490));
        assert_eq!(e.publish_date.as_deref(), Some("Aug 25, 2017"));
        // The key the second request is made against. No work key, no request.
        assert_eq!(
            e.works.unwrap()[0].key.as_deref(),
            Some("/works/OL9000001W")
        );
    }
}
