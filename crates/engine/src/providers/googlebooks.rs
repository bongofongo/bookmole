use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use url::Url;

use super::{
    Manners, MetadataProvider, ProviderBook, ProviderId, SearchRequest, normalize_subjects,
    retry_after, year_of_date,
};
use crate::book::{Book, normalize_isbn};
use crate::error::{EngineError, Result};

pub fn scrub_key(msg: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"key=[^&\s]+").expect("static regex"))
        .replace_all(msg, "key=REDACTED")
        .into_owned()
}

/// A request's failure naming neither the key nor the query: reqwest puts the whole URL in its
/// error text, and the query is the reader's private reading.
fn scrubbed(e: reqwest::Error) -> EngineError {
    EngineError::Provider {
        provider: ProviderId::GoogleBooks,
        message: scrub_key(&e.without_url().to_string()),
    }
}

/// Live-checks an API key. `Err` carries a human-readable reason and never the key itself.
pub async fn verify_key(key: &str) -> std::result::Result<(), String> {
    let client = Client::new();
    let mut url = Url::parse("https://www.googleapis.com/books/v1/volumes").expect("static url");
    url.query_pairs_mut()
        .append_pair("q", "the")
        .append_pair("maxResults", "1")
        .append_pair("key", key);
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| scrub_key(&format!("network error: {e}")))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_default();
    let reason = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| status.to_string());
    Err(scrub_key(&reason))
}

const DEFAULT_BASE: &str = "https://www.googleapis.com/books/v1/volumes";

pub struct GoogleBooksProvider {
    client: Client,
    api_key: Option<String>,
    base: String,
    /// An `Arc` so the engine keeps one across provider rebuilds: a key change must not forget a wait
    /// Google asked for.
    manners: Arc<Manners>,
}

impl GoogleBooksProvider {
    pub fn new(client: Client, api_key: Option<String>) -> Self {
        Self::with_base(client, api_key, DEFAULT_BASE)
    }

    #[doc(hidden)]
    pub fn with_base(client: Client, api_key: Option<String>, base: impl Into<String>) -> Self {
        GoogleBooksProvider {
            client,
            api_key,
            base: base.into(),
            manners: Arc::new(Manners::google_books()),
        }
    }

    pub fn with_manners(mut self, manners: Arc<Manners>) -> Self {
        self.manners = manners;
        self
    }

    /// One GET, refused without asking while Google has asked us to wait; a 429 or 503 closes it
    /// for the stated time. The error carries neither the URL nor the key.
    async fn get_text(&self, url: String) -> Result<String> {
        if self.manners.closed_for().is_some() {
            return Err(EngineError::Provider {
                provider: ProviderId::GoogleBooks,
                message: "asked to wait; not asked again yet".into(),
            });
        }
        self.manners.wait_turn().await;
        let resp = self.client.get(url).send().await.map_err(scrubbed)?;
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
        {
            if let Some(after) = retry_after(resp.headers()) {
                self.manners.close_for(after);
            }
            return Err(EngineError::Provider {
                provider: ProviderId::GoogleBooks,
                message: format!("HTTP {status}"),
            });
        }
        resp.error_for_status()
            .map_err(scrubbed)?
            .text()
            .await
            .map_err(scrubbed)
    }
}

#[derive(Deserialize, Debug)]
struct VolumesResp {
    items: Option<Vec<Volume>>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Volume {
    id: Option<String>,
    volume_info: Option<VolumeInfo>,
}

#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct VolumeInfo {
    title: Option<String>,
    subtitle: Option<String>,
    authors: Option<Vec<String>>,
    publisher: Option<String>,
    published_date: Option<String>,
    description: Option<String>,
    industry_identifiers: Option<Vec<IndustryId>>,
    page_count: Option<i64>,
    language: Option<String>,
    image_links: Option<ImageLinks>,
    /// BISAC-shaped paths such as `Fiction / Literary`; stored as published.
    categories: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct IndustryId {
    #[serde(rename = "type")]
    kind: String,
    identifier: String,
}

/// Every size Google documents for `volumeInfo.imageLinks`; larger ones are absent for volumes
/// without a bigger scan, hence the fallback chain in [`ImageLinks::best`].
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ImageLinks {
    extra_large: Option<String>,
    large: Option<String>,
    medium: Option<String>,
    small: Option<String>,
    thumbnail: Option<String>,
    small_thumbnail: Option<String>,
}

impl ImageLinks {
    /// The largest published size, as `https`, with `edge=curl` dropped: it composites a page-curl
    /// graphic onto the jacket, and the stored cover must be a clean scan.
    fn best(&self) -> Option<String> {
        [
            &self.extra_large,
            &self.large,
            &self.medium,
            &self.small,
            &self.thumbnail,
            &self.small_thumbnail,
        ]
        .into_iter()
        .flatten()
        .next()
        .map(|u| u.replace("http://", "https://"))
        .map(|u| strip_param(&u, "edge"))
    }
}

/// Removes one `key=value` pair from a query string, leaving the rest byte-for-byte. Not
/// `Url::query_pairs_mut`, which re-escapes the others and changes the stored `cover_url`.
fn strip_param(url: &str, key: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|p| p.split('=').next().unwrap_or(p) != key)
        .collect();
    if kept.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", kept.join("&"))
    }
}

impl Volume {
    fn to_book(&self) -> Book {
        let info = self.volume_info.as_ref();
        let mut book = Book {
            googlebooks_id: self.id.clone(),
            ..Default::default()
        };
        let Some(info) = info else { return book };

        book.title = match (&info.title, &info.subtitle) {
            (Some(t), Some(s)) => Some(format!("{t}: {s}")),
            (Some(t), None) => Some(t.clone()),
            _ => None,
        };
        book.authors = info.authors.clone().unwrap_or_default();
        book.publisher = info.publisher.clone();
        book.publish_year = info.published_date.as_deref().and_then(year_of_date);
        book.description = info.description.clone();
        book.page_count = info.page_count;
        book.language = info.language.as_ref().map(|l| l.to_lowercase());
        book.subjects = normalize_subjects(info.categories.iter().flatten());
        // No series: `volumeInfo` has no documented series field; series comes from Open Library.
        book.cover_url = info.image_links.as_ref().and_then(ImageLinks::best);
        for iid in info.industry_identifiers.iter().flatten() {
            if let Some(norm) = normalize_isbn(&iid.identifier) {
                match iid.kind.as_str() {
                    "ISBN_10" if book.isbn_10.is_none() => book.isbn_10 = Some(norm),
                    "ISBN_13" if book.isbn_13.is_none() => book.isbn_13 = Some(norm),
                    _ => {}
                }
            }
        }
        book
    }
}

fn build_query(req: &SearchRequest) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(q) = &req.query {
        parts.push(q.clone());
    }
    if let Some(t) = &req.title {
        parts.push(format!("intitle:\"{t}\""));
    }
    if let Some(a) = &req.author {
        parts.push(format!("inauthor:\"{a}\""));
    }
    if let Some(tr) = &req.translator {
        // Google has no translator facet; translators appear among authors, so fold into inauthor.
        parts.push(format!("inauthor:\"{tr}\""));
    }
    if let Some(p) = &req.publisher {
        parts.push(format!("inpublisher:\"{p}\""));
    }
    if let Some(i) = &req.isbn {
        parts.push(format!("isbn:{i}"));
    }
    parts.join(" ")
}

fn build_url(base: &str, req: &SearchRequest, api_key: Option<&str>) -> Result<String> {
    let limit = if req.limit == 0 {
        30
    } else {
        req.limit.min(40)
    };
    let mut url = Url::parse(base)?;
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("q", &build_query(req));
        qp.append_pair("maxResults", &limit.to_string());
        qp.append_pair("printType", "books");
        if let Some(l) = &req.language {
            qp.append_pair("langRestrict", l);
        }
        if let Some(k) = api_key {
            qp.append_pair("key", k);
        }
    }
    Ok(url.into())
}

#[async_trait]
impl MetadataProvider for GoogleBooksProvider {
    fn id(&self) -> ProviderId {
        ProviderId::GoogleBooks
    }

    fn manners(&self) -> Option<&Manners> {
        Some(&self.manners)
    }

    async fn search(&self, req: &SearchRequest) -> Result<Vec<ProviderBook>> {
        let url = build_url(&self.base, req, self.api_key.as_deref())?;
        let text = self.get_text(url).await?;
        let resp: VolumesResp = serde_json::from_str(&text)?;
        let mut out: Vec<ProviderBook> = resp
            .items
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(position, v)| ProviderBook {
                book: v.to_book(),
                provider: ProviderId::GoogleBooks,
                position,
            })
            .collect();
        // langRestrict can be flaky, so also filter by year client-side.
        if let Some(y) = req.year {
            out.retain(|pb| pb.book.publish_year.is_none() || pb.book.publish_year == Some(y));
        }
        Ok(out)
    }

    async fn by_isbn(&self, isbn: &str) -> Result<Option<ProviderBook>> {
        let req = SearchRequest {
            isbn: Some(isbn.to_string()),
            limit: 1,
            ..Default::default()
        };
        Ok(self.search(&req).await?.into_iter().next())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_composition() {
        let req = SearchRequest {
            title: Some("The Trial".into()),
            author: Some("Kafka".into()),
            translator: Some("Breon Mitchell".into()),
            publisher: Some("Schocken".into()),
            ..Default::default()
        };
        let q = build_query(&req);
        assert!(q.contains("intitle:\"The Trial\""));
        assert!(q.contains("inauthor:\"Kafka\""));
        assert!(q.contains("inauthor:\"Breon Mitchell\""));
        assert!(q.contains("inpublisher:\"Schocken\""));
    }

    #[test]
    fn scrub_redacts_api_keys() {
        let msg = "HTTP status client error (429) for url \
                   (https://www.googleapis.com/books/v1/volumes?q=dune&key=AIzaSySECRET123&maxResults=15)";
        let s = scrub_key(msg);
        assert!(!s.contains("AIzaSySECRET123"));
        assert!(s.contains("key=REDACTED"));
        assert!(s.contains("q=dune"));
    }

    #[test]
    fn key_appended_only_when_set() {
        let req = SearchRequest {
            query: Some("dune".into()),
            ..Default::default()
        };
        let without = build_url(DEFAULT_BASE, &req, None).unwrap();
        assert!(!without.contains("key="));
        let with = build_url(DEFAULT_BASE, &req, Some("SECRET")).unwrap();
        assert!(with.contains("key=SECRET"));
    }

    #[test]
    fn a_full_request_maps_to_an_exact_url() {
        let req = SearchRequest {
            query: Some("dune".into()),
            title: Some("Dune".into()),
            author: Some("Frank Herbert".into()),
            publisher: Some("Ace".into()),
            isbn: Some("9780441013593".into()),
            language: Some("en".into()),
            limit: 10,
            ..Default::default()
        };
        let url = build_url(DEFAULT_BASE, &req, None).unwrap();
        assert_eq!(
            url,
            "https://www.googleapis.com/books/v1/volumes\
             ?q=dune+intitle%3A%22Dune%22+inauthor%3A%22Frank+Herbert%22\
             +inpublisher%3A%22Ace%22+isbn%3A9780441013593\
             &maxResults=10&printType=books&langRestrict=en"
        );
    }

    #[test]
    fn limits_are_clamped_to_what_the_api_accepts() {
        let mk = |limit| SearchRequest {
            query: Some("x".into()),
            limit,
            ..Default::default()
        };
        assert!(
            build_url(DEFAULT_BASE, &mk(0), None)
                .unwrap()
                .contains("maxResults=30")
        );
        assert!(
            build_url(DEFAULT_BASE, &mk(1000), None)
                .unwrap()
                .contains("maxResults=40")
        );
        assert!(
            build_url(DEFAULT_BASE, &mk(7), None)
                .unwrap()
                .contains("maxResults=7")
        );
    }

    #[test]
    fn volume_deserializes_and_converts() {
        let json = r#"{
            "items": [{
                "id": "abc123",
                "volumeInfo": {
                    "title": "Middlemarch",
                    "authors": ["George Eliot"],
                    "publisher": "William Blackwood and Sons",
                    "publishedDate": "2017-02-07",
                    "description": "A saga.",
                    "industryIdentifiers": [
                        {"type": "ISBN_13", "identifier": "9781900000178"},
                        {"type": "ISBN_10", "identifier": "1900000172"}
                    ],
                    "pageCount": 490,
                    "language": "en",
                    "categories": ["Fiction / Literary", "  Fiction / Literary  ", "", "History"],
                    "imageLinks": {"thumbnail": "http://books.google.com/x.jpg"}
                }
            }]
        }"#;
        let resp: VolumesResp = serde_json::from_str(json).unwrap();
        let b = resp.items.unwrap()[0].to_book();
        assert_eq!(b.googlebooks_id.as_deref(), Some("abc123"));
        assert_eq!(b.isbn_13.as_deref(), Some("9781900000178"));
        assert_eq!(b.publish_year, Some(2017));
        assert_eq!(b.description.as_deref(), Some("A saga."));
        assert!(b.cover_url.unwrap().starts_with("https://"));
        assert_eq!(b.subjects, vec!["Fiction / Literary", "History"]);
        assert_eq!(b.series, None);
    }

    #[test]
    fn a_volume_without_categories_claims_no_subjects() {
        let json = r#"{"items": [{"id": "x", "volumeInfo": {"title": "T"}}]}"#;
        let resp: VolumesResp = serde_json::from_str(json).unwrap();
        assert!(resp.items.unwrap()[0].to_book().subjects.is_empty());
    }

    #[test]
    fn the_cover_url_is_the_largest_size_google_published() {
        let mk = |body: &str| {
            let json = format!(
                r#"{{"items": [{{"id": "x", "volumeInfo": {{"title": "T", "imageLinks": {body}}}}}]}}"#
            );
            serde_json::from_str::<VolumesResp>(&json)
                .unwrap()
                .items
                .unwrap()[0]
                .to_book()
                .cover_url
        };
        assert_eq!(
            mk(
                r#"{"smallThumbnail": "https://x/s", "thumbnail": "https://x/t",
                   "small": "https://x/1", "medium": "https://x/2",
                   "large": "https://x/3", "extraLarge": "https://x/4"}"#
            )
            .as_deref(),
            Some("https://x/4")
        );
        assert_eq!(
            mk(r#"{"smallThumbnail": "https://x/s", "thumbnail": "https://x/t"}"#).as_deref(),
            Some("https://x/t")
        );
        assert_eq!(
            mk(r#"{"smallThumbnail": "https://x/s"}"#).as_deref(),
            Some("https://x/s")
        );
        assert_eq!(mk("{}"), None);
    }

    #[test]
    fn the_page_curl_is_stripped_and_nothing_else_is_touched() {
        let url = "http://books.google.com/books/content?id=AAA&printsec=frontcover\
                   &img=1&zoom=1&edge=curl&source=gbs_api";
        let links: ImageLinks =
            serde_json::from_str(&format!(r#"{{"extraLarge": "{url}"}}"#)).unwrap();
        let got = links.best().unwrap();
        assert!(got.starts_with("https://"), "{got}");
        assert!(!got.contains("edge=curl"), "{got}");
        assert_eq!(
            got,
            "https://books.google.com/books/content?id=AAA&printsec=frontcover\
             &img=1&zoom=1&source=gbs_api"
                .replace(char::is_whitespace, "")
        );
        assert_eq!(strip_param("https://x/y.jpg", "edge"), "https://x/y.jpg");
        assert_eq!(strip_param("https://x/y?edge=curl", "edge"), "https://x/y");
    }

    #[tokio::test]
    async fn retry_after_closes_google_and_no_url_or_key_leaks() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "30"))
            .expect(1)
            .mount(&server)
            .await;
        let gb = GoogleBooksProvider::with_base(
            Client::builder().no_proxy().build().unwrap(),
            Some("AIzaSyPRIVATEKEY".into()),
            format!("{}/books/v1/volumes", server.uri()),
        );
        let req = SearchRequest {
            query: Some("a private query".into()),
            ..Default::default()
        };
        let text = gb.search(&req).await.unwrap_err().to_string();
        assert!(text.contains("429"), "{text}");
        for secret in ["private", "AIzaSy", "volumes"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
        assert!(gb.manners().and_then(Manners::closed_for).is_some());
        let again = gb.search(&req).await.unwrap_err().to_string();
        assert!(!again.contains("private"), "{again}");
        server.verify().await;
    }

    #[tokio::test]
    async fn a_transport_failure_names_neither_the_query_nor_the_key() {
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            drop(l);
            format!("http://127.0.0.1:{port}/books/v1/volumes")
        };
        let gb = GoogleBooksProvider::with_base(
            Client::builder().no_proxy().build().unwrap(),
            Some("AIzaSyPRIVATEKEY".into()),
            dead,
        );
        let req = SearchRequest {
            query: Some("a private query".into()),
            ..Default::default()
        };
        let err = gb.search(&req).await.unwrap_err();
        let d = crate::diagnostic::Diagnostic::provider_failed(ProviderId::GoogleBooks, &err);
        for text in [err.to_string(), d.to_string()] {
            for secret in ["private", "AIzaSy", "127.0.0.1"] {
                assert!(!text.contains(secret), "{secret} in {text}");
            }
        }
        assert!(d.to_string().starts_with("googlebooks: "), "{d}");
    }
}
