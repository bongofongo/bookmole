pub mod googlebooks;
pub mod openlibrary;

use async_trait::async_trait;

use crate::book::Book;
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderId {
    OpenLibrary,
    GoogleBooks,
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ProviderId {
    /// Every provider this build knows, in the order a search lists them.
    pub const ALL: [ProviderId; 2] = [ProviderId::OpenLibrary, ProviderId::GoogleBooks];

    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::OpenLibrary => "openlibrary",
            ProviderId::GoogleBooks => "googlebooks",
        }
    }

    /// Inverse of [`ProviderId::as_str`]; `None` for a word from a newer build.
    pub fn parse(word: &str) -> Option<ProviderId> {
        Self::ALL.into_iter().find(|p| p.as_str() == word)
    }

    pub fn defaults(self) -> &'static ProviderDefaults {
        PROVIDER_DEFAULTS
            .iter()
            .find(|d| d.id == self)
            .expect("every provider has a row in PROVIDER_DEFAULTS")
    }
}

const DAY: std::time::Duration = std::time::Duration::from_secs(86_400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDefaults {
    pub id: ProviderId,
    /// On when the reader has said nothing (no `provider_settings` row); a keyed provider is on only
    /// once it has a key.
    pub on_by_default: bool,
    pub needs_key: bool,
    /// How long a record may be kept. `None`: never, because its terms allow no durable copy.
    pub kept_for: Option<std::time::Duration>,
}

pub const PROVIDER_DEFAULTS: &[ProviderDefaults] = &[
    ProviderDefaults {
        id: ProviderId::OpenLibrary,
        on_by_default: true,
        needs_key: false,
        kept_for: Some(std::time::Duration::from_secs(180 * DAY.as_secs())),
    },
    ProviderDefaults {
        id: ProviderId::GoogleBooks,
        // On once a key is set: without one it is not built, and keyless Google answers 429 to every call.
        on_by_default: true,
        needs_key: true,
        kept_for: None,
    },
];

/// The reader's choice where made, else the default. A keyed provider is never built without a
/// key, whatever this says ([`ProviderAvailability::NoKey`]).
pub fn provider_enabled(id: ProviderId, chosen: Option<bool>) -> bool {
    chosen.unwrap_or(id.defaults().on_by_default)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAvailability {
    Ready,
    Off,
    NoKey,
}

pub fn availability(id: ProviderId, chosen: Option<bool>, has_key: bool) -> ProviderAvailability {
    if !provider_enabled(id, chosen) {
        ProviderAvailability::Off
    } else if id.defaults().needs_key && !has_key {
        ProviderAvailability::NoKey
    } else {
        ProviderAvailability::Ready
    }
}

impl From<ProviderId> for crate::storage::Source {
    /// An exhaustive `match`, not a string from `Display`: a new provider must be a compile error here,
    /// not a source `field_provenance` silently fails to recognise.
    fn from(id: ProviderId) -> Self {
        match id {
            ProviderId::OpenLibrary => crate::storage::Source::OpenLibrary,
            ProviderId::GoogleBooks => crate::storage::Source::GoogleBooks,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchRequest {
    pub query: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub publisher: Option<String>,
    /// Neither API has a translator field; providers map it to their closest contributor facility.
    pub translator: Option<String>,
    pub language: Option<String>,
    pub year: Option<i64>,
    pub isbn: Option<String>,
    pub limit: u32,
}

impl SearchRequest {
    pub fn is_empty(&self) -> bool {
        self.query.is_none()
            && self.title.is_none()
            && self.author.is_none()
            && self.publisher.is_none()
            && self.translator.is_none()
            && self.isbn.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct ProviderBook {
    pub book: Book,
    pub provider: ProviderId,
    pub position: usize,
}

/// What a provider says about the work a result stands for. Read by ranking; never written to a
/// book.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkFacts {
    pub edition_count: Option<u32>,
    /// Other titles the work answers to: the matched edition's title and titles with subtitles, so a
    /// translated title can find the work.
    pub also_titled: Vec<String>,
    pub series: Option<String>,
    pub series_position: Option<f64>,
    pub languages: Vec<String>,
    /// The author asked for (folded) when the provider returned this work: its own word that the work
    /// is theirs, which matters when it files the name differently.
    pub found_by_author: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderWork {
    pub found: ProviderBook,
    pub facts: WorkFacts,
    /// The records this result may be kept as. Empty for a provider whose records are never kept, and
    /// for a result not read off a response.
    pub kept: Vec<KeptRecord>,
}

impl ProviderWork {
    pub fn bare(found: ProviderBook) -> Self {
        ProviderWork {
            found,
            facts: WorkFacts::default(),
            kept: Vec::new(),
        }
    }

    pub fn work_key(&self) -> Option<&str> {
        match self.found.provider {
            ProviderId::OpenLibrary => self
                .found
                .book
                .openlibrary_key
                .as_deref()
                .filter(|k| k.starts_with("/works/")),
            ProviderId::GoogleBooks => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Work,
    Edition,
}

impl RecordKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RecordKind::Work => "work",
            RecordKind::Edition => "edition",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordForm {
    Doc,
    /// The engine's reading of an answer assembled from several requests, where no one response exists.
    Book,
}

impl RecordForm {
    pub fn as_str(self) -> &'static str {
        match self {
            RecordForm::Doc => "doc",
            RecordForm::Book => "book",
        }
    }

    pub fn parse(word: &str) -> Option<RecordForm> {
        match word {
            "doc" => Some(RecordForm::Doc),
            "book" => Some(RecordForm::Book),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeptRecord {
    pub key: String,
    pub kind: RecordKind,
    pub form: RecordForm,
    /// For the offline index only.
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub body: String,
}

#[async_trait]
pub trait MetadataProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    async fn search(&self, req: &SearchRequest) -> Result<Vec<ProviderBook>>;
    async fn by_isbn(&self, isbn: &str) -> Result<Option<ProviderBook>>;

    async fn search_works(&self, req: &SearchRequest) -> Result<Vec<ProviderWork>> {
        Ok(self
            .search(req)
            .await?
            .into_iter()
            .map(ProviderWork::bare)
            .collect())
    }

    /// The provider's rate limit and closed state, if it keeps one; the fan-out reports a closed
    /// provider as a state rather than asking again.
    fn manners(&self) -> Option<&Manners> {
        None
    }
}

pub const MAX_CLOSED: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// A provider's rate limit and *closed until* state. A token bucket: `burst` requests at once,
/// tokens return at `per_second`, and a request without one waits inside the caller's search
/// budget. A 429 or 503 with `Retry-After` closes it (capped at [`MAX_CLOSED`]) instead of being
/// retried in a loop.
#[derive(Debug)]
pub struct Manners {
    per_second: f64,
    burst: f64,
    state: std::sync::Mutex<MannersState>,
}

#[derive(Debug)]
struct MannersState {
    tokens: f64,
    refilled: Option<tokio::time::Instant>,
    closed_until: Option<tokio::time::Instant>,
}

impl Manners {
    /// A `per_second` that is not positive and finite means no limit.
    pub fn new(per_second: f64, burst: u32) -> Self {
        let burst = f64::from(burst.max(1));
        Manners {
            per_second,
            burst,
            state: std::sync::Mutex::new(MannersState {
                tokens: burst,
                refilled: None,
                closed_until: None,
            }),
        }
    }

    /// No rate limit, but a `Retry-After` still closes it; for providers pointed at a test server.
    pub fn unlimited() -> Self {
        Self::new(f64::INFINITY, 1)
    }

    /// One request a second, its published anonymous rate, with a burst of three so an ISBN lookup's
    /// requests are not serialised.
    pub fn open_library() -> Self {
        Self::new(1.0, 3)
    }

    pub fn google_books() -> Self {
        Self::new(2.0, 4)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MannersState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn limited(&self) -> bool {
        self.per_second.is_finite() && self.per_second > 0.0
    }

    pub async fn wait_turn(&self) {
        if !self.limited() {
            return;
        }
        loop {
            let wait = {
                let mut s = self.lock();
                let now = tokio::time::Instant::now();
                if let Some(last) = s.refilled {
                    let gained = now.duration_since(last).as_secs_f64() * self.per_second;
                    s.tokens = (s.tokens + gained).min(self.burst);
                }
                s.refilled = Some(now);
                if s.tokens >= 1.0 {
                    s.tokens -= 1.0;
                    return;
                }
                std::time::Duration::from_secs_f64((1.0 - s.tokens) / self.per_second)
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// Capped at [`MAX_CLOSED`]; a shorter later answer does not shorten a longer one in force.
    pub fn close_for(&self, after: std::time::Duration) {
        let until = tokio::time::Instant::now() + after.min(MAX_CLOSED);
        let mut s = self.lock();
        if s.closed_until.is_none_or(|was| was < until) {
            s.closed_until = Some(until);
        }
    }

    pub fn closed_for(&self) -> Option<std::time::Duration> {
        let mut s = self.lock();
        let now = tokio::time::Instant::now();
        match s.closed_until {
            Some(until) if until > now => Some(until - now),
            Some(_) => {
                s.closed_until = None;
                None
            }
            None => None,
        }
    }
}

pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    let raw = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(std::time::Duration::from_secs(secs));
    }
    let at =
        time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc2822).ok()?;
    let left = at - time::OffsetDateTime::now_utc();
    Some(if left.is_positive() {
        std::time::Duration::from_secs(left.whole_seconds().unsigned_abs())
    } else {
        std::time::Duration::ZERO
    })
}

/// Trims, drops empties, and dedups case-insensitively keeping the first spelling. Shape only,
/// never vocabulary, and no cap: providers' subject vocabularies differ, and truncating would be
/// silent.
pub fn normalize_subjects<I, S>(raw: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for s in raw {
        let t = s.as_ref().split_whitespace().collect::<Vec<_>>().join(" ");
        if t.is_empty() {
            continue;
        }
        if !out.iter().any(|kept| kept.eq_ignore_ascii_case(&t)) {
            out.push(t);
        }
    }
    out
}

/// OpenLibrary speaks MARC 3-letter codes, Google BCP-47 2-letter; stores 2-letter where known.
pub fn normalize_language(raw: &str) -> String {
    let l = raw.trim().to_lowercase();
    match l.as_str() {
        "eng" => "en",
        "fre" | "fra" => "fr",
        "ger" | "deu" => "de",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "rus" => "ru",
        "jpn" => "ja",
        "chi" | "zho" => "zh",
        "kor" => "ko",
        "ara" => "ar",
        "dut" | "nld" => "nl",
        "swe" => "sv",
        "nor" => "no",
        "dan" => "da",
        "fin" => "fi",
        "pol" => "pl",
        "tur" => "tr",
        "heb" => "he",
        "lat" => "la",
        "grc" | "gre" | "ell" => "el",
        other => other,
    }
    .to_string()
}

pub fn to_marc_language(raw: &str) -> String {
    let l = raw.trim().to_lowercase();
    match l.as_str() {
        "en" => "eng",
        "fr" => "fre",
        "de" => "ger",
        "es" => "spa",
        "it" => "ita",
        "pt" => "por",
        "ru" => "rus",
        "ja" => "jpn",
        "zh" => "chi",
        "ko" => "kor",
        "ar" => "ara",
        "nl" => "dut",
        "sv" => "swe",
        "no" => "nor",
        "da" => "dan",
        "fi" => "fin",
        "pl" => "pol",
        "tr" => "tur",
        "he" => "heb",
        "la" => "lat",
        "el" => "gre",
        other => other,
    }
    .to_string()
}

pub fn year_of_date(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i - start == 4 {
                return s[start..i].parse().ok();
            }
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn year_extraction() {
        assert_eq!(year_of_date("Aug 25, 2017"), Some(2017));
        assert_eq!(year_of_date("2017-08-25"), Some(2017));
        assert_eq!(year_of_date("2017"), Some(2017));
        assert_eq!(year_of_date("25th"), None);
        assert_eq!(year_of_date(""), None);
    }

    #[tokio::test(start_paused = true)]
    async fn the_bucket_lets_a_burst_through_and_then_keeps_the_rate() {
        let m = Manners::new(1.0, 3);
        let start = tokio::time::Instant::now();
        for _ in 0..3 {
            m.wait_turn().await;
        }
        assert_eq!(
            start.elapsed(),
            std::time::Duration::ZERO,
            "the burst waited"
        );
        m.wait_turn().await;
        let waited = start.elapsed();
        assert!(
            waited >= std::time::Duration::from_millis(999)
                && waited <= std::time::Duration::from_millis(1001),
            "{waited:?}"
        );
        for _ in 0..10 {
            m.wait_turn().await;
        }
        let total = start.elapsed().as_secs_f64();
        assert!((total - 11.0).abs() < 0.01, "{total}");
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let idle = tokio::time::Instant::now();
        for _ in 0..3 {
            m.wait_turn().await;
        }
        assert_eq!(idle.elapsed(), std::time::Duration::ZERO);
        m.wait_turn().await;
        assert!(idle.elapsed() >= std::time::Duration::from_millis(999));
    }

    #[tokio::test(start_paused = true)]
    async fn unlimited_never_waits() {
        let m = Manners::unlimited();
        let start = tokio::time::Instant::now();
        for _ in 0..100 {
            m.wait_turn().await;
        }
        assert_eq!(start.elapsed(), std::time::Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_closes_for_its_time_capped() {
        let m = Manners::open_library();
        assert_eq!(m.closed_for(), None);
        m.close_for(std::time::Duration::from_secs(30));
        assert_eq!(m.closed_for(), Some(std::time::Duration::from_secs(30)));
        m.close_for(std::time::Duration::from_secs(5));
        assert_eq!(m.closed_for(), Some(std::time::Duration::from_secs(30)));
        tokio::time::sleep(std::time::Duration::from_secs(31)).await;
        assert_eq!(m.closed_for(), None);

        m.close_for(std::time::Duration::from_secs(86_400));
        assert_eq!(m.closed_for(), Some(MAX_CLOSED));
    }

    #[test]
    fn retry_after_reads_seconds_and_dates() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
        let mut h = HeaderMap::new();
        assert_eq!(retry_after(&h), None);
        h.insert(RETRY_AFTER, HeaderValue::from_static("120"));
        assert_eq!(retry_after(&h), Some(std::time::Duration::from_secs(120)));
        h.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after(&h), Some(std::time::Duration::ZERO));
        h.insert(RETRY_AFTER, HeaderValue::from_static("soon"));
        assert_eq!(retry_after(&h), None);
    }

    #[test]
    fn language_roundtrip() {
        assert_eq!(normalize_language("eng"), "en");
        assert_eq!(normalize_language("EN"), "en");
        assert_eq!(to_marc_language("en"), "eng");
        assert_eq!(normalize_language("xyz"), "xyz");
    }
}
