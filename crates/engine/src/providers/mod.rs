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

    /// The word for this provider in the database (`provider_records`,
    /// `search_cache`, `provider_settings`) and in a diagnostic's text.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::OpenLibrary => "openlibrary",
            ProviderId::GoogleBooks => "googlebooks",
        }
    }

    /// The inverse of [`ProviderId::as_str`]; `None` for a word this build does
    /// not know (a row written by a newer one).
    pub fn parse(word: &str) -> Option<ProviderId> {
        Self::ALL.into_iter().find(|p| p.as_str() == word)
    }

    /// This provider's row of [`PROVIDER_DEFAULTS`].
    pub fn defaults(self) -> &'static ProviderDefaults {
        PROVIDER_DEFAULTS
            .iter()
            .find(|d| d.id == self)
            .expect("every provider has a row in PROVIDER_DEFAULTS")
    }
}

// ---- the defaults table -----------------------------------------------------
//
// Keyless providers are on out of the box; one that needs the reader's own key
// is opt-in by having one. Every provider can be switched off.

/// A day, for the keeps below.
const DAY: std::time::Duration = std::time::Duration::from_secs(86_400);

/// What a provider is out of the box, and how long its answers may be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDefaults {
    pub id: ProviderId,
    /// Switched on when the reader has said nothing (`provider_settings` has no
    /// row). Keyless providers are; a provider that needs a key is on only once
    /// it has one, which [`ProviderDefaults::needs_key`] says.
    pub on_by_default: bool,
    /// Built at all only with the reader's own key or token.
    pub needs_key: bool,
    /// How long one of its records may be kept (`provider_records`). `None`:
    /// never kept — its terms allow no durable copy, so its records live in the
    /// session's memory and nowhere else.
    pub kept_for: Option<std::time::Duration>,
}

/// One row per provider. A provider added later is a row here and no
/// migration: `provider_settings` holds only what the reader changed.
pub const PROVIDER_DEFAULTS: &[ProviderDefaults] = &[
    ProviderDefaults {
        id: ProviderId::OpenLibrary,
        on_by_default: true,
        needs_key: false,
        kept_for: Some(std::time::Duration::from_secs(180 * DAY.as_secs())),
    },
    ProviderDefaults {
        id: ProviderId::GoogleBooks,
        // On once a key is set: without one it is not built, and keyless
        // Google answers 429 `RESOURCE_EXHAUSTED` to every call anyway
        // (checked 2026-11-12).
        on_by_default: true,
        needs_key: true,
        // "Must not keep a durable copy": session memory only.
        kept_for: None,
    },
];

/// Whether a provider is switched on: the reader's own choice where they made
/// one, else the default. A provider that needs a key is never *built* without
/// one, whatever this says — that is [`ProviderAvailability::NoKey`].
pub fn provider_enabled(id: ProviderId, chosen: Option<bool>) -> bool {
    chosen.unwrap_or(id.defaults().on_by_default)
}

/// Why a provider is or is not in the list a search asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAvailability {
    /// Built, and asked by every search.
    Ready,
    /// Switched off by the reader (or off by default). Sent nothing.
    Off,
    /// On, but it needs a key or token and has none. Sent nothing.
    NoKey,
}

/// [`ProviderAvailability`] for one provider, from the reader's choice and
/// whether a key is present.
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
    /// A provider, as `field_provenance` names it.
    ///
    /// An exhaustive `match` and not a string built from [`Display`], though the
    /// two spell the same words today: `Source`'s vocabulary is wider than this
    /// enum (calibre, epub, koreader, goodreads, the user) and going through
    /// text would let a fourth provider land as a source nothing in
    /// `field_provenance` recognises, silently. Here it is a compile error.
    fn from(id: ProviderId) -> Self {
        match id {
            ProviderId::OpenLibrary => crate::storage::Source::OpenLibrary,
            ProviderId::GoogleBooks => crate::storage::Source::GoogleBooks,
        }
    }
}

/// Fielded search request; any combination of fields may be set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchRequest {
    /// Free-text query.
    pub query: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub publisher: Option<String>,
    /// Neither API has a first-class translator field; providers map this to
    /// their closest contributor/author facility and ranking rewards matches.
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

/// One raw result from one provider, position preserved for ranking.
#[derive(Debug, Clone)]
pub struct ProviderBook {
    pub book: Book,
    pub provider: ProviderId,
    pub position: usize,
}

/// What a provider says about the **work** a search result stands for, beside
/// the edition-shaped [`Book`] it returns.
///
/// Ranking reads these (`search::ranking`); nothing here is written to a book.
/// Kept off [`ProviderBook`] so that type's literals (in every provider and in
/// the engine's tests) keep compiling, and off [`Book`] because none of it is a
/// column: an edition count is a fact about a provider's catalogue, and the
/// title of the edition a provider matched is a ranking hint, not the book's
/// title.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkFacts {
    /// How many editions the provider holds of this work. The *standing*
    /// signal: a log of it breaks ties between equally good text matches.
    pub edition_count: Option<u32>,
    /// Other titles the work answers to: the title of the edition the
    /// provider matched to the query (Open Library's nested best edition), and
    /// titles with their subtitles (`Perfume: the story of a murderer`). This
    /// is how *The Name of the Rose* finds a work whose own title is *Il nome
    /// della rosa*.
    pub also_titled: Vec<String>,
    /// The series the provider files the work under, and its place in it.
    pub series: Option<String>,
    pub series_position: Option<f64>,
    /// Languages the work's editions come in, two-letter where known.
    pub languages: Vec<String>,
    /// The author a provider was asked for (folded) when it answered with
    /// this work. The provider's own word that the work is theirs, which
    /// matters when it files the name differently (`村上春樹` for *Haruki
    /// Murakami*).
    pub found_by_author: Option<String>,
}

/// One search result with its [`WorkFacts`]. What
/// [`MetadataProvider::search_works`] returns.
#[derive(Debug, Clone)]
pub struct ProviderWork {
    pub found: ProviderBook,
    pub facts: WorkFacts,
    /// The records this result may be kept as: the provider's own
    /// JSON for the work and the edition it matched. Empty for a provider whose
    /// records are never kept (Google Books), and for a result made here rather
    /// than read off a response.
    pub kept: Vec<KeptRecord>,
}

impl ProviderWork {
    /// A result from a provider with no work model: no facts beyond the book.
    pub fn bare(found: ProviderBook) -> Self {
        ProviderWork {
            found,
            facts: WorkFacts::default(),
            kept: Vec::new(),
        }
    }

    /// The provider's own id for the **work** this result stands for, when it
    /// has a work model: Open Library's `/works/…W`. Two results naming the same
    /// one are one work: the first and strongest grouping rule.
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

/// Whether a kept record is a work or an edition (`provider_records.kind`).
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

/// How a kept record's body is read (`provider_records.form`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordForm {
    /// The provider's own JSON for this record, as it sent it.
    Doc,
    /// The engine's reading of an answer that took several requests to
    /// assemble (an ISBN lookup: edition, authors, work), where there is no one
    /// response to keep.
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

/// One record a provider sent, as it may be kept (`provider_records`).
#[derive(Debug, Clone, PartialEq)]
pub struct KeptRecord {
    /// The provider's own id for the record.
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

    /// [`MetadataProvider::search`] with what the provider knows about each
    /// result's work. A provider with no work model keeps this default.
    async fn search_works(&self, req: &SearchRequest) -> Result<Vec<ProviderWork>> {
        Ok(self
            .search(req)
            .await?
            .into_iter()
            .map(ProviderWork::bare)
            .collect())
    }

    /// This provider's rate limit and its *come back later* state, when it
    /// keeps one. The fan-out reads it to report a closed provider as a state
    /// rather than ask it again.
    fn manners(&self) -> Option<&Manners> {
        None
    }
}

// ---- manners ----------------------------------------------------------------
//
// One shared HTTP client, a token bucket per provider at its published rate,
// and a 429 or 503 with `Retry-After` closing the provider for the stated time
// (capped at `MAX_CLOSED`) — reported as a state, never retried in a loop. A
// search that would exceed the rate waits inside the provider's budget
// (`search::PROVIDER_TIMEOUT`) and then degrades.

/// The longest a `Retry-After` is honoured for. A provider asking for a day is
/// asked again after this, and refused again if it meant it.
pub const MAX_CLOSED: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// A provider's rate limit and its *closed until* state, shared by every
/// request the provider makes.
///
/// **A token bucket**: `burst` requests may go at once, and tokens come back at
/// `per_second`. A request with no token waits for one — inside the caller's
/// [`crate::search`] budget, which is what turns a search that would exceed the
/// rate into a timeout rather than a 429. On `tokio::time`, so it is tested
/// under `start_paused` in no wall time.
///
/// **Closed** is a provider having said *come back later* (a 429 or 503 with
/// `Retry-After`): it is not asked again until then, capped at [`MAX_CLOSED`],
/// and the fan-out reports it as `ProviderRateLimited` instead of retrying in a
/// loop.
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
    /// `per_second` tokens a second, at most `burst` held. A `per_second` that
    /// is not positive and finite means no limit.
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

    /// No rate limit; a `Retry-After` still closes it. For a provider pointed
    /// at a loopback server in a test, which has no rate to keep.
    pub fn unlimited() -> Self {
        Self::new(f64::INFINITY, 1)
    }

    /// Open Library: one request a second, its published anonymous rate, with
    /// a burst of three so an ISBN lookup's edition, author and work requests
    /// are not serialised a second apart. Sustained, it is still one a second.
    pub fn open_library() -> Self {
        Self::new(1.0, 3)
    }

    /// Google Books, with the reader's own key: two requests a second, a burst
    /// of four. Google publishes a daily quota per key (1,000 by default) and a
    /// per-user rate rather than one number; this is well under both for a
    /// person searching, and it is what keeps a key's quota from being spent
    /// by anything faster than a person.
    pub fn google_books() -> Self {
        Self::new(2.0, 4)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MannersState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn limited(&self) -> bool {
        self.per_second.is_finite() && self.per_second > 0.0
    }

    /// Wait for a token and take it.
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

    /// The provider said to come back after `after`. Capped at [`MAX_CLOSED`];
    /// a later, shorter answer does not shorten a longer one already in force.
    pub fn close_for(&self, after: std::time::Duration) {
        let until = tokio::time::Instant::now() + after.min(MAX_CLOSED);
        let mut s = self.lock();
        if s.closed_until.is_none_or(|was| was < until) {
            s.closed_until = Some(until);
        }
    }

    /// How much longer the provider is closed, or `None` when it is open.
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

/// A `Retry-After` header's value: delta-seconds, or an HTTP date. `None` when
/// absent or unreadable, which is "no Retry-After" and closes nothing.
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

/// Tidy a provider's subject list: trim, drop the empties, drop repeats.
///
/// **Shape only, never vocabulary.** The strings stay exactly as the provider
/// spelled them — Google's BISAC paths (`Fiction / Literary`) and OpenLibrary's
/// headings (`Provincial England`) are two different systems of naming and
/// mapping one onto the other is a *controlled vocabulary*, which is the same
/// question `book_tags` deferred and is not answerable from two examples. The
/// merge sidesteps it by taking one provider's set whole rather than mixing
/// them; see `MERGE_RULES`.
///
/// Dedup is case-insensitive and keeps the **first** spelling, because a
/// provider that says both `Fiction` and `fiction` said one thing twice and the
/// order it published is the only ranking available.
///
/// **No cap.** OpenLibrary works genuinely carry a hundred subjects, many of
/// them cataloguing artifacts (`Accessible book`, `Protected DAISY`,
/// `In library`), and dropping the tail would be a silent truncation of exactly
/// the sort this repo bans elsewhere — a caller cannot tell a trimmed list from
/// a short one. Filtering the artifacts *is* a vocabulary decision, and it is
/// left undone on purpose so it can be made against real data rather than
/// against a guess.
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

/// Normalize language tags across providers: OpenLibrary speaks MARC
/// 3-letter codes ("eng"), Google Books BCP-47 2-letter ("en"). We store
/// 2-letter where known, pass through otherwise.
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

/// Inverse mapping for building OpenLibrary queries from 2-letter input.
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

/// Extract the first 4-digit year from a free-form date string
/// ("Aug 25, 2017", "2017-08-25", "2017").
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

    /// The bucket under paused time: a burst goes at once, the next waits for
    /// its token, and the wait is the rate's.
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
        // Ten more take ten more seconds: sustained, it is the rate.
        for _ in 0..10 {
            m.wait_turn().await;
        }
        let total = start.elapsed().as_secs_f64();
        assert!((total - 11.0).abs() < 0.01, "{total}");
        // An idle stretch refills to the burst and no further.
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

    /// Closed for what was asked, capped at ten minutes, and open again after.
    #[tokio::test(start_paused = true)]
    async fn retry_after_closes_for_its_time_capped() {
        let m = Manners::open_library();
        assert_eq!(m.closed_for(), None);
        m.close_for(std::time::Duration::from_secs(30));
        assert_eq!(m.closed_for(), Some(std::time::Duration::from_secs(30)));
        // A shorter answer does not shorten it.
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
        // A date in the past is "now", not an error.
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
