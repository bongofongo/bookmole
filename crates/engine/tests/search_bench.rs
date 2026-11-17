//! The search benchmark: a category's figures may never drop below `baseline.json`.
//! Providers are replayed from recorded responses over loopback; nothing here reaches the
//! network except `BOOKMOLE_SEARCH_RECORD=1`.
//! One test function on purpose: the run shares one engine and one replay server.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bookmole::providers::googlebooks::{GoogleBooksProvider, scrub_key};
use bookmole::providers::openlibrary::OpenLibraryProvider;
use bookmole::providers::{Manners, MetadataProvider};
use bookmole::search::wellknown::Loader;
use bookmole::{
    Book, CopyForm, Engine, EngineConfig, LearnedFrom, NewCopy, SearchRequest, SearchScope, Tenure,
    normalize_isbn,
};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const BENCH_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/search/bench");

/// App name and version only: a request must carry nothing about the person or machine.
const USER_AGENT: &str = concat!("bookmole/", env!("CARGO_PKG_VERSION"));

/// Open Library's published anonymous rate is one a second; the margin is for clock jitter.
const LIVE_GAP: Duration = Duration::from_millis(1100);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BenchRow {
    title: Option<String>,
    authors: Vec<String>,
    ol_key: Option<String>,
    isbn_10: Option<String>,
    isbn_13: Option<String>,
    wikidata: Option<String>,
    held: Option<i64>,
}

impl BenchRow {
    fn of(book: Book, held: Option<i64>) -> Self {
        BenchRow {
            title: book.title,
            authors: book.authors,
            ol_key: book.openlibrary_key,
            isbn_10: book.isbn_10,
            isbn_13: book.isbn_13,
            wikidata: None,
            held,
        }
    }
}

/// Every baseline figure comes through this function, called as a frontend's add-a-book box
/// calls it: trimmed free-text `query`, fielded parameters `None`, `limit: 20`. An ISBN is
/// typed into the same box.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Answer {
    rows: Vec<BenchRow>,
    warnings: Vec<String>,
}

/// All categories but *you already have it* use `bench_answer`; that one reads each work row's
/// `Held::Certain`/`Held::Likely`, because the flattened row's `shelf_id` misses works held
/// under no shared ISBN.
async fn bench_search(engine: &Engine, q: &BenchQuery) -> Answer {
    match q.category.as_str() {
        HELD if q.scope.as_deref() == Some(LIBRARY) => bench_library(engine, &q.query).await,
        HELD => bench_held(engine, &q.query).await,
        _ => bench_answer(engine, &q.query).await,
    }
}

async fn bench_library(engine: &Engine, query: &str) -> Answer {
    Answer {
        warnings: Vec::new(),
        rows: engine
            .search_library(query.trim(), 20)
            .await
            .expect("search the library")
            .into_iter()
            .map(|m| BenchRow {
                held: Some(m.book_id),
                ..Default::default()
            })
            .collect(),
    }
}

async fn bench_held(engine: &Engine, query: &str) -> Answer {
    let mut snap = engine
        .start_search(query.trim(), SearchScope::Everything, 20)
        .await
        .expect("start a search");
    while !snap.done {
        snap = engine
            .poll_search(&snap.session, snap.revision, Duration::from_millis(1500))
            .await
            .expect("poll a search");
    }
    engine.end_search(&snap.session);
    Answer {
        warnings: snap.warnings.iter().map(|d| d.to_string()).collect(),
        rows: snap
            .works
            .into_iter()
            .map(|r| BenchRow::of(r.work.flattened().clone(), r.work.held.book_id()))
            .collect(),
    }
}

async fn bench_answer(engine: &Engine, query: &str) -> Answer {
    let req = SearchRequest {
        query: Some(query.trim().to_string()),
        limit: 20,
        ..Default::default()
    };
    let outcome = engine.search(&req).await.expect("search");
    Answer {
        warnings: outcome.warnings.iter().map(|d| d.to_string()).collect(),
        rows: outcome
            .results
            .into_iter()
            .map(|r| BenchRow::of(r.book, r.shelf_id))
            .collect(),
    }
}

#[derive(Debug, Deserialize)]
struct QuerySet {
    library: Vec<Seed>,
    queries: Vec<BenchQuery>,
}

#[derive(Debug, Deserialize)]
struct Seed {
    key: String,
    title: String,
    authors: Vec<String>,
    #[serde(default)]
    isbn_13: Option<String>,
    ol_work: String,
}

#[derive(Debug, Deserialize)]
struct BenchQuery {
    id: String,
    category: String,
    query: String,
    #[serde(default = "yes")]
    gates: bool,
    /// `library`: ask the library's own search instead of the add-a-book search.
    #[serde(default)]
    scope: Option<String>,
    expect: Expect,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct Expect {
    #[serde(default, deserialize_with = "one_or_many")]
    ol_work: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    wikidata: Vec<String>,
    #[serde(default)]
    isbns: Vec<String>,
    /// Fallback identity, with `author`: compared folded (case, diacritics, punctuation, one
    /// leading article); a translated title is right too.
    #[serde(default, deserialize_with = "one_or_many")]
    title: Vec<String>,
    #[serde(default, deserialize_with = "one_or_many")]
    author: Vec<String>,
    #[serde(default)]
    held: Option<String>,
}

fn one_or_many<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

const AUTHOR_ONLY: &str = "author_only";
const HELD: &str = "held";
const LIBRARY: &str = "library";

const CATEGORY_ORDER: &[&str] = &[
    "typos",
    "title_author",
    "series",
    "translations",
    "non_english",
    "recent",
    "isbn",
    "partial",
    AUTHOR_ONLY,
    "cjk_cyrillic",
    HELD,
];

/// Deliberately not the engine's folding: a scorer borrowing the code it scores agrees by
/// construction.
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => out.push('a'),
            'ç' | 'ć' | 'č' => out.push('c'),
            'ď' | 'đ' => out.push('d'),
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => out.push('i'),
            'ł' | 'ľ' | 'ĺ' => out.push('l'),
            'ñ' | 'ń' | 'ň' => out.push('n'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => out.push('o'),
            'ŕ' | 'ř' => out.push('r'),
            'ś' | 'š' | 'ş' => out.push('s'),
            'ť' | 'ţ' => out.push('t'),
            'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'ź' | 'ż' | 'ž' => out.push('z'),
            'ß' => out.push_str("ss"),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            'ё' => out.push('е'),
            // An apostrophe joins (`philosopher's` is one word).
            '\'' | '’' | 'ʼ' => {}
            c if c.is_alphanumeric() => out.push(c),
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// [`fold`], less one leading English article, judged before folding so French `À` is not `a`.
fn fold_title(s: &str) -> String {
    let s = s.trim();
    let first = s.split_whitespace().next().unwrap_or_default();
    match first.to_lowercase().as_str() {
        "the" | "a" | "an" => fold(&s[first.len()..]),
        _ => fold(s),
    }
}

/// One folded name inside the other (spaces removed), or the same surname and first initial:
/// `J.R.R. Tolkien` matches `John Ronald Reuel Tolkien`, `Frank Herbert` not `Brian Herbert`.
fn same_author(expected: &[String], got: &str) -> bool {
    let g: String = fold(got).chars().filter(|c| !c.is_whitespace()).collect();
    if g.is_empty() {
        return false;
    }
    expected.iter().any(|e| {
        let folded = fold(e);
        let joined: String = folded.chars().filter(|c| !c.is_whitespace()).collect();
        if joined.is_empty() {
            return false;
        }
        if g.contains(&joined) || joined.contains(&g) {
            return true;
        }
        let surname = folded.split(' ').next_back().unwrap_or_default();
        surname.chars().count() > 1
            && g.contains(surname)
            && g.chars().next() == joined.chars().next()
    })
}

fn by_author(expect: &Expect, row: &BenchRow) -> bool {
    row.authors.iter().any(|a| same_author(&expect.author, a))
}

fn isbn13_of(raw: &str) -> Option<String> {
    let n = normalize_isbn(raw)?;
    if n.len() == 13 {
        Some(n)
    } else {
        bookmole::isbn10_to_13(&n)
    }
}

fn is_the_book(expect: &Expect, row: &BenchRow) -> bool {
    if let Some(k) = &row.ol_key
        && expect.ol_work.iter().any(|w| w == k)
    {
        return true;
    }
    if let Some(w) = &row.wikidata
        && expect.wikidata.contains(w)
    {
        return true;
    }
    let wanted: HashSet<String> = expect.isbns.iter().filter_map(|i| isbn13_of(i)).collect();
    if [&row.isbn_13, &row.isbn_10]
        .into_iter()
        .flatten()
        .filter_map(|i| isbn13_of(i))
        .any(|i| wanted.contains(&i))
    {
        return true;
    }
    let Some(have) = &row.title else {
        return false;
    };
    let have = fold_title(have);
    expect.title.iter().any(|want| fold_title(want) == have)
        && (expect.author.is_empty() || row.authors.is_empty() || by_author(expect, row))
}

#[derive(Debug, Clone, Copy, Default)]
struct Score {
    at1: bool,
    /// In the first five (for *author only*: four of the first five are hers).
    top5: bool,
    rr: f64,
}

fn score(
    q: &BenchQuery,
    rows: &[BenchRow],
    held_ids: &HashMap<String, i64>,
) -> (Score, Option<usize>) {
    let rank = rows
        .iter()
        .position(|row| match q.category.as_str() {
            HELD => {
                let key = q
                    .expect
                    .held
                    .as_deref()
                    .expect("a held query names its seed");
                row.held.is_some() && row.held == held_ids.get(key).copied()
            }
            AUTHOR_ONLY => by_author(&q.expect, row),
            _ => is_the_book(&q.expect, row),
        })
        .map(|i| i + 1);
    let top5 = if q.category == AUTHOR_ONLY {
        rows.iter()
            .take(5)
            .filter(|r| by_author(&q.expect, r))
            .count()
            >= 4
    } else {
        rank.is_some_and(|r| r <= 5)
    };
    let s = Score {
        at1: rank == Some(1),
        top5,
        rr: rank.map_or(0.0, |r| 1.0 / r as f64),
    };
    (s, rank)
}

/// Rounded to four places so the ratchet never trips on float noise.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
struct Figures {
    n: usize,
    at1: f64,
    top5: f64,
    mrr: f64,
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

fn figures(scores: &[Score]) -> Figures {
    let n = scores.len();
    if n == 0 {
        return Figures::default();
    }
    let frac = |k: usize| round4(k as f64 / n as f64);
    Figures {
        n,
        at1: frac(scores.iter().filter(|s| s.at1).count()),
        top5: frac(scores.iter().filter(|s| s.top5).count()),
        mrr: round4(scores.iter().map(|s| s.rr).sum::<f64>() / n as f64),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Baseline {
    measured: String,
    /// Gated categories plus `overall`; reported-only categories are absent.
    figures: BTreeMap<String, Figures>,
}

struct Host {
    name: &'static str,
    prefix: &'static str,
    live: &'static str,
}

const OPEN_LIBRARY: Host = Host {
    name: "openlibrary",
    prefix: "/openlibrary",
    live: "https://openlibrary.org",
};
const GOOGLE_BOOKS: Host = Host {
    name: "googlebooks",
    prefix: "/googlebooks",
    live: "https://www.googleapis.com",
};
const HOSTS: [&Host; 2] = [&OPEN_LIBRARY, &GOOGLE_BOOKS];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    file: String,
    status: u16,
    content_type: String,
    recorded: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    recordings: BTreeMap<String, Entry>,
}

/// `live` carries the URL as built, key included, and is never written anywhere.
#[derive(Debug, Clone)]
struct Miss {
    host: &'static str,
    url: String,
    live: String,
}

#[derive(Default)]
struct Tape {
    requests: HashMap<&'static str, usize>,
    indexes: HashMap<&'static str, Index>,
    bodies: HashMap<String, Vec<u8>>,
    used: HashSet<String>,
    misses: Vec<Miss>,
}

impl Tape {
    fn load(all: bool) -> Tape {
        let mut tape = Tape::default();
        for host in HOSTS {
            let dir = recorded_dir(host);
            let index: Index = if all {
                Index::default()
            } else {
                std::fs::read_to_string(dir.join("index.json"))
                    .map(|s| serde_json::from_str(&s).expect("index.json parses"))
                    .unwrap_or_default()
            };
            for (url, e) in &index.recordings {
                let body = std::fs::read(dir.join(&e.file))
                    .unwrap_or_else(|err| panic!("{}: {err}", dir.join(&e.file).display()));
                tape.bodies.insert(url.clone(), body);
            }
            tape.indexes.insert(host.name, index);
        }
        tape
    }

    fn recorded(&self, host: &Host) -> bool {
        self.indexes
            .get(host.name)
            .is_some_and(|i| !i.recordings.is_empty())
    }
}

fn recorded_dir(host: &Host) -> PathBuf {
    Path::new(BENCH_DIR).join("recorded").join(host.name)
}

fn canonical(url: &url::Url) -> Option<(&'static Host, String, String)> {
    let path = url.path();
    let host = HOSTS.into_iter().find(|h| path.starts_with(h.prefix))?;
    let mut live = format!("{}{}", host.live, &path[host.prefix.len()..]);
    if let Some(q) = url.query() {
        live.push('?');
        live.push_str(q);
    }
    Some((host, scrub_key(&live), live))
}

struct Replay(Arc<Mutex<Tape>>);

impl Respond for Replay {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let mut tape = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let Some((host, url, live)) = canonical(&req.url) else {
            let url = req.url.to_string();
            tape.misses.push(Miss {
                host: "unknown",
                url: url.clone(),
                live: url,
            });
            return ResponseTemplate::new(501).set_body_string("no such provider");
        };
        *tape.requests.entry(host.name).or_default() += 1;
        let entry = tape
            .indexes
            .get(host.name)
            .and_then(|i| i.recordings.get(&url))
            .cloned();
        match (entry, tape.bodies.get(&url).cloned()) {
            (Some(e), Some(body)) => {
                tape.used.insert(url);
                ResponseTemplate::new(e.status).set_body_raw(body, &e.content_type)
            }
            _ => {
                let body = format!("not recorded: {url}");
                tape.misses.push(Miss {
                    host: host.name,
                    url,
                    live,
                });
                ResponseTemplate::new(501).set_body_string(body)
            }
        }
    }
}

struct Recorder {
    client: reqwest::Client,
    last: HashMap<&'static str, Instant>,
    google_key: Option<String>,
    today: String,
}

impl Recorder {
    fn new(google_key: Option<String>) -> Recorder {
        let today = time::OffsetDateTime::now_utc().date().to_string();
        Recorder {
            client: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(30))
                .build()
                .expect("client"),
            last: HashMap::new(),
            google_key,
            today,
        }
    }

    async fn pace(&mut self, host: &'static str) {
        if let Some(at) = self.last.get(host) {
            let since = at.elapsed();
            if since < LIVE_GAP {
                tokio::time::sleep(LIVE_GAP - since).await;
            }
        }
        self.last.insert(host, Instant::now());
    }

    /// A 429 stops the run: *slow down* is not a recording of the provider's answer.
    async fn get(&mut self, host: &'static str, url: &str) -> (u16, String, Vec<u8>) {
        let mut last_err = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            self.pace(host).await;
            match self.client.get(url).send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let content_type = resp
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("application/octet-stream")
                        .to_string();
                    if status == 429 {
                        panic!(
                            "{host} answered 429 to {}; stopping, nothing more recorded",
                            scrub_key(url)
                        );
                    }
                    if status >= 500 {
                        last_err = format!("status {status}");
                        continue;
                    }
                    match resp.bytes().await {
                        Ok(b) => return (status, content_type, b.to_vec()),
                        Err(e) => last_err = scrub_key(&e.to_string()),
                    }
                }
                Err(e) => last_err = scrub_key(&e.to_string()),
            }
        }
        panic!(
            "{host} could not be reached for {} ({last_err}); not recorded",
            scrub_key(url)
        );
    }

    async fn record(&mut self, tape: &Arc<Mutex<Tape>>, miss: &Miss) {
        let host = HOSTS
            .into_iter()
            .find(|h| h.name == miss.host)
            .unwrap_or_else(|| panic!("a request to no known provider: {}", miss.url));
        if host.name == GOOGLE_BOOKS.name && self.google_key.is_none() {
            panic!("asked Google Books without a key: {}", miss.url);
        }
        let (status, content_type, body) = self.get(host.name, &miss.live).await;
        let hash = hex(&Sha256::digest(miss.url.as_bytes()));
        let ext = if content_type.contains("json") {
            "json"
        } else {
            "txt"
        };
        let file = format!("{}.{ext}", &hash[..16]);
        let dir = recorded_dir(host);
        std::fs::create_dir_all(&dir).expect("recorded dir");
        std::fs::write(dir.join(&file), &body).expect("write recording");
        let mut t = tape.lock().unwrap_or_else(|e| e.into_inner());
        t.bodies.insert(miss.url.clone(), body);
        let index = t.indexes.entry(host.name).or_default();
        index.recordings.insert(
            miss.url.clone(),
            Entry {
                file,
                status,
                content_type,
                recorded: self.today.clone(),
            },
        );
        write_index(host, index);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_index(host: &Host, index: &Index) {
    let dir = recorded_dir(host);
    std::fs::create_dir_all(&dir).expect("recorded dir");
    let text = serde_json::to_string_pretty(index).expect("index serialises");
    std::fs::write(dir.join("index.json"), text + "\n").expect("write index");
}

fn prune(tape: &mut Tape) {
    for host in HOSTS {
        let Some(index) = tape.indexes.get_mut(host.name) else {
            continue;
        };
        index.recordings.retain(|url, _| tape.used.contains(url));
        let dir = recorded_dir(host);
        if index.recordings.is_empty() {
            if dir.exists() {
                std::fs::remove_dir_all(&dir).expect("remove empty recorded dir");
            }
            continue;
        }
        write_index(host, index);
        let keep: HashSet<&str> = index.recordings.values().map(|e| e.file.as_str()).collect();
        for f in std::fs::read_dir(&dir).expect("recorded dir").flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if name != "index.json" && !keep.contains(name.as_str()) {
                std::fs::remove_file(f.path()).expect("remove stale recording");
            }
        }
    }
}

async fn verify_live(set: &QuerySet, tape: &Arc<Mutex<Tape>>, rec: &mut Recorder) {
    let mut wanted: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for q in &set.queries {
        for w in &q.expect.ol_work {
            wanted
                .entry(w.clone())
                .or_default()
                .extend(q.expect.author.clone());
        }
    }
    for s in &set.library {
        wanted
            .entry(s.ol_work.clone())
            .or_default()
            .extend(s.authors.clone());
    }
    let keys: Vec<&String> = wanted.keys().collect();
    let mut problems = Vec::new();
    for chunk in keys.chunks(40) {
        let ors = chunk
            .iter()
            .map(|k| format!("\"{k}\""))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut url = url::Url::parse("https://openlibrary.org/search.json").expect("url");
        url.query_pairs_mut()
            .append_pair("q", &format!("key:({ors})"))
            .append_pair("fields", "key,title,author_name")
            .append_pair("limit", "100");
        let (status, _, body) = rec.get(OPEN_LIBRARY.name, url.as_str()).await;
        assert_eq!(status, 200, "verification search failed");
        let v: serde_json::Value = serde_json::from_slice(&body).expect("search json");
        let docs = v["docs"].as_array().cloned().unwrap_or_default();
        for k in chunk {
            let doc = docs.iter().find(|d| d["key"].as_str() == Some(k.as_str()));
            let Some(doc) = doc else {
                problems.push(format!("{k}: not a work Open Library knows"));
                continue;
            };
            let authors: Vec<String> = doc["author_name"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let expected = &wanted[*k];
            if !expected.is_empty() && !authors.iter().any(|a| same_author(expected, a)) {
                problems.push(format!(
                    "{k} ({}): by {authors:?}, expected {expected:?}",
                    doc["title"]
                ));
            }
        }
    }
    for q in set.queries.iter().filter(|q| q.category == "isbn") {
        let isbn = normalize_isbn(&q.query).expect("an isbn query is an isbn");
        let url = format!("https://openlibrary.org/isbn/{isbn}.json");
        let body = tape
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .bodies
            .get(&url)
            .cloned();
        match body {
            Some(b) => check_edition_work(&q.id, &b, &q.expect.ol_work, &mut problems),
            None => problems.push(format!("{}: {url} was not recorded", q.id)),
        }
    }
    for s in &set.library {
        if let Some(isbn) = &s.isbn_13 {
            let url = format!("https://openlibrary.org/isbn/{isbn}.json");
            let (status, _, body) = rec.get(OPEN_LIBRARY.name, &url).await;
            if status != 200 {
                problems.push(format!("seed {}: {url} answered {status}", s.key));
                continue;
            }
            check_edition_work(
                &format!("seed {}", s.key),
                &body,
                std::slice::from_ref(&s.ol_work),
                &mut problems,
            );
        }
    }
    assert!(
        problems.is_empty(),
        "expected identities that live Open Library does not bear out:\n  {}",
        problems.join("\n  ")
    );
}

fn check_edition_work(what: &str, body: &[u8], works: &[String], problems: &mut Vec<String>) {
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            problems.push(format!("{what}: edition does not parse ({e})"));
            return;
        }
    };
    let of: Vec<&str> = v["works"]
        .as_array()
        .map(|a| a.iter().filter_map(|w| w["key"].as_str()).collect())
        .unwrap_or_default();
    if !of.iter().any(|w| works.iter().any(|x| x == w)) {
        problems.push(format!("{what}: edition is of {of:?}, expected {works:?}"));
    }
}

fn env_on(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.is_empty() && v != "0")
}

async fn engine() -> (tempfile::TempDir, Engine) {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Built literally: `EngineConfig::rooted_at` would read the ambient `GOOGLE_BOOKS_API_KEY`.
    let config = EngineConfig {
        db_url: "sqlite::memory:".into(),
        images_dir: tmp.path().join("database/images"),
        files_dir: tmp.path().join("database/files"),
        vault_dir: tmp.path().join("vault"),
        log_dir: tmp.path().join("logs"),
        exports_dir: tmp.path().join("exports"),
        google_api_key: None,
        calibre_bin_dir: None,
    };
    let engine = Engine::open(config).await.expect("engine opens");
    (tmp, engine)
}

/// Each seed has a copy, so each is in the library by the copies rule.
async fn seed_library(engine: &Engine, seeds: &[Seed]) -> HashMap<String, i64> {
    let mut ids = HashMap::new();
    for s in seeds {
        let book = Book {
            title: Some(s.title.clone()),
            authors: s.authors.clone(),
            isbn_13: s.isbn_13.clone(),
            ..Default::default()
        };
        let saved = engine.save_book(&book).await.expect("seed a book");
        let id = saved.id.expect("a saved book has an id");
        engine
            .add_copy(&NewCopy::new(
                id,
                Some(CopyForm::Print),
                Tenure::Own,
                LearnedFrom::User,
            ))
            .await
            .expect("seed a copy");
        ids.insert(s.key.clone(), id);
    }
    ids
}

enum Outcome {
    Scored(Score, Option<usize>, Vec<BenchRow>),
    Unrecorded(Vec<String>),
}

#[tokio::test]
async fn search_bench() {
    let record = env_on("BOOKMOLE_SEARCH_RECORD");
    let all = record && env_on("ALL");
    let verbose = env_on("SEARCH_BENCH_VERBOSE");
    let set: QuerySet = serde_json::from_str(
        &std::fs::read_to_string(Path::new(BENCH_DIR).join("queries.json")).expect("queries.json"),
    )
    .expect("queries.json parses");

    let tape = Arc::new(Mutex::new(Tape::load(all)));
    if !record && !tape.lock().unwrap().recorded(&OPEN_LIBRARY) {
        if env_on("BOOKMOLE_REQUIRE_FIXTURES") {
            panic!("no Open Library recordings under {BENCH_DIR}/recorded");
        }
        eprintln!(
            "SKIPPED: search_bench — no recordings under {BENCH_DIR}/recorded \
             (make search-bench-record)"
        );
        return;
    }

    // Google is asked only when recorded or recording with a key; keyless calls are all 429s,
    // so its half is left out rather than failed.
    let google_key = std::env::var("GOOGLE_BOOKS_API_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    let with_google = if record {
        google_key.is_some()
    } else {
        tape.lock().unwrap().recorded(&GOOGLE_BOOKS)
    };

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(Replay(tape.clone()))
        .mount(&server)
        .await;
    // `no_proxy`: the ambient environment must not route a loopback request elsewhere.
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("client");
    // No rate limit on the replay; while recording, the `Recorder` keeps the live rate itself.
    let mut providers: Vec<Box<dyn MetadataProvider>> = vec![Box::new(
        OpenLibraryProvider::with_base(
            client.clone(),
            format!("{}{}", server.uri(), OPEN_LIBRARY.prefix),
        )
        .with_manners(Arc::new(Manners::unlimited())),
    )];
    if with_google {
        let key = if record {
            google_key.clone()
        } else {
            Some("replayed".into())
        };
        providers.push(Box::new(GoogleBooksProvider::with_base(
            client.clone(),
            key,
            format!("{}{}/books/v1/volumes", server.uri(), GOOGLE_BOOKS.prefix),
        )));
    }

    let (_tmp, engine) = engine().await;
    engine.replace_providers(providers);
    // The shipped offline index, built once into the target scratch dir: the real index, not a
    // fixture.
    let index_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("wellknown");
    engine.use_wellknown(Loader::Embedded {
        dir: index_dir.clone(),
    });
    let built = Instant::now();
    let probe = engine.suggest_works("dune", 1).await.expect("suggest");
    assert!(
        probe.warnings.is_empty(),
        "the offline index did not open: {:?}",
        probe.warnings
    );
    let opened_in = built.elapsed();
    let held_ids = seed_library(&engine, &set.library).await;

    let mut recorder = record.then(|| Recorder::new(google_key.clone()));
    let mut outcomes: Vec<(&BenchQuery, Outcome)> = Vec::new();
    for q in &set.queries {
        let mut attempts = 0;
        let outcome = loop {
            tape.lock().unwrap().misses.clear();
            let rows = bench_search(&engine, q).await.rows;
            let misses = std::mem::take(&mut tape.lock().unwrap().misses);
            if misses.is_empty() {
                let (s, rank) = score(q, &rows, &held_ids);
                break Outcome::Scored(s, rank, rows);
            }
            let Some(rec) = recorder.as_mut() else {
                break Outcome::Unrecorded(misses.into_iter().map(|m| m.url).collect());
            };
            attempts += 1;
            assert!(
                attempts < 8,
                "{}: still missing after {attempts} passes",
                q.id
            );
            let mut seen = HashSet::new();
            for m in misses.iter().filter(|m| seen.insert(m.url.clone())) {
                rec.record(&tape, m).await;
            }
        };
        outcomes.push((q, outcome));
    }

    if let Some(rec) = recorder.as_mut() {
        prune(&mut tape.lock().unwrap());
        verify_live(&set, &tape, rec).await;
    }

    let mut by_cat: BTreeMap<&str, Vec<Score>> = BTreeMap::new();
    let mut gated_cat: BTreeMap<&str, bool> = BTreeMap::new();
    let mut unrecorded = Vec::new();
    for (q, o) in &outcomes {
        *gated_cat.entry(q.category.as_str()).or_insert(true) &= q.gates;
        match o {
            Outcome::Scored(s, rank, rows) => {
                by_cat.entry(q.category.as_str()).or_default().push(*s);
                if verbose {
                    let top: Vec<String> = rows
                        .iter()
                        .take(3)
                        .map(|r| {
                            format!(
                                "{} / {}",
                                r.title.as_deref().unwrap_or("?"),
                                r.authors.first().map_or("?", String::as_str)
                            )
                        })
                        .collect();
                    println!(
                        "{:<16} {:<44} {:>4}  {}",
                        q.id,
                        q.query,
                        rank.map_or("-".to_string(), |r| r.to_string()),
                        top.join(" | ")
                    );
                }
            }
            Outcome::Unrecorded(urls) => {
                for u in urls {
                    unrecorded.push(format!("{}: {u}", q.id));
                }
            }
        }
    }
    let known: HashSet<&str> = CATEGORY_ORDER.iter().copied().collect();
    for c in gated_cat.keys() {
        assert!(known.contains(c), "a query in an unknown category: {c}");
    }

    let mut current: BTreeMap<String, Figures> = BTreeMap::new();
    let mut gated_scores = Vec::new();
    let mut all_scores = Vec::new();
    println!();
    println!(
        "search benchmark — bench_search over replayed recordings (openlibrary: recorded; googlebooks: {})",
        if with_google {
            "recorded"
        } else {
            "not recorded, left out"
        }
    );
    println!(
        "{:<14} {:>4}  {:>6}  {:>6}  {:>6}  gates",
        "category", "n", "@1", "top5", "MRR"
    );
    for cat in CATEGORY_ORDER {
        let Some(scores) = by_cat.get(cat) else {
            continue;
        };
        let f = figures(scores);
        let gates = gated_cat[cat];
        println!(
            "{:<14} {:>4}  {:>6.3}  {:>6.3}  {:>6.3}  {}",
            cat,
            f.n,
            f.at1,
            f.top5,
            f.mrr,
            if gates { "yes" } else { "reported" }
        );
        all_scores.extend(scores.iter().copied());
        if gates {
            gated_scores.extend(scores.iter().copied());
            current.insert(cat.to_string(), f);
        }
    }
    let overall = figures(&gated_scores);
    let everything = figures(&all_scores);
    current.insert("overall".to_string(), overall);
    for (label, f) in [("overall", overall), ("all, reported", everything)] {
        println!(
            "{:<14} {:>4}  {:>6.3}  {:>6.3}  {:>6.3}",
            label, f.n, f.at1, f.top5, f.mrr
        );
    }
    println!();

    assert!(
        unrecorded.is_empty(),
        "{} request(s) had no recording — run `make search-bench-record`:\n  {}",
        unrecorded.len(),
        unrecorded.join("\n  ")
    );

    if !record {
        let mut fired: BTreeMap<&str, usize> = BTreeMap::new();
        let mut false_fixes = Vec::new();
        for (q, o) in &outcomes {
            if !matches!(o, Outcome::Scored(..)) || q.scope.as_deref() == Some(LIBRARY) {
                continue;
            }
            let mut snap = engine
                .start_search(q.query.trim(), SearchScope::Everything, 20)
                .await
                .expect("start a search");
            while !snap.done {
                snap = engine
                    .poll_search(&snap.session, snap.revision, Duration::from_millis(1500))
                    .await
                    .expect("poll a search");
            }
            engine.end_search(&snap.session);
            if let Some(c) = &snap.correction {
                *fired.entry(q.category.as_str()).or_default() += 1;
                if q.category != "typos" {
                    false_fixes.push(format!("{} {:?} -> {:?}", q.id, q.query, c.searched));
                }
            }
        }
        let mut as_typed: BTreeMap<&str, Vec<Score>> = BTreeMap::new();
        for (q, o) in &outcomes {
            if !matches!(o, Outcome::Scored(..)) || q.category == HELD {
                continue;
            }
            let req = SearchRequest {
                query: Some(q.query.trim().to_string()),
                limit: 20,
                ..Default::default()
            };
            let rows: Vec<BenchRow> = engine
                .search_as_typed(&req)
                .await
                .expect("search as typed")
                .results
                .into_iter()
                .map(|r| BenchRow::of(r.book, r.shelf_id))
                .collect();
            as_typed
                .entry(q.category.as_str())
                .or_default()
                .push(score(q, &rows, &held_ids).0);
        }
        for cat in ["typos", "translations", "series"] {
            if let Some(scores) = as_typed.get(cat) {
                let f = figures(scores);
                println!(
                    "{:<14} {:>4}  {:>6.3}  {:>6.3}  {:>6.3}  as typed, never corrected (reported)",
                    cat, f.n, f.at1, f.top5, f.mrr
                );
            }
        }
        let on_typos = fired.get("typos").copied().unwrap_or(0);
        println!(
            "corrections    {on_typos} on typos, {} elsewhere (false){}",
            false_fixes.len(),
            if false_fixes.is_empty() {
                String::new()
            } else {
                format!(": {}", false_fixes.join("; "))
            }
        );
        println!(
            "offline index  opened in {:.2?} ({})",
            opened_in,
            if opened_in > Duration::from_secs(1) {
                "built this run"
            } else {
                "reopened"
            }
        );
        println!();
        assert!(
            false_fixes.is_empty(),
            "a query that found its book was corrected: {false_fixes:?}"
        );
    }

    if !record {
        let first: Vec<(&BenchQuery, &Vec<BenchRow>)> = outcomes
            .iter()
            .filter_map(|(q, o)| match o {
                Outcome::Scored(_, _, rows) => Some((*q, rows)),
                Outcome::Unrecorded(_) => None,
            })
            .collect();
        let gated = |scores: &[(bool, Score)]| {
            figures(
                &scores
                    .iter()
                    .filter(|(g, _)| *g)
                    .map(|(_, s)| *s)
                    .collect::<Vec<_>>(),
            )
        };

        let asked_before = tape.lock().unwrap().requests.get("openlibrary").copied();
        let mut repeat = Vec::new();
        let mut moved = Vec::new();
        for (q, rows) in &first {
            let again = bench_search(&engine, q).await.rows;
            if &again != *rows {
                moved.push(q.id.clone());
            }
            repeat.push((q.gates, score(q, &again, &held_ids).0));
        }
        let asked = tape
            .lock()
            .unwrap()
            .requests
            .get("openlibrary")
            .copied()
            .unwrap_or(0)
            - asked_before.unwrap_or(0);
        let f = gated(&repeat);
        println!(
            "{:<14} {:>4}  {:>6.3}  {:>6.3}  {:>6.3}  open library asked {asked} times",
            "repeat", f.n, f.at1, f.top5, f.mrr
        );

        engine
            .storage()
            .age_kept_searches(2 * 86_400)
            .await
            .expect("age the kept searches");
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
            let port = l.local_addr().expect("its address").port();
            drop(l);
            format!("http://127.0.0.1:{port}")
        };
        engine.replace_providers(vec![Box::new(
            OpenLibraryProvider::with_base(
                client.clone(),
                format!("{dead}{}", OPEN_LIBRARY.prefix),
            )
            .with_manners(Arc::new(Manners::unlimited())),
        )]);
        let mut offline = Vec::new();
        let mut differ = Vec::new();
        let mut unsaid = Vec::new();
        for (q, rows) in &first {
            let a = bench_search(&engine, q).await;
            if &a.rows != *rows {
                differ.push(q.id.clone());
            }
            if q.scope.as_deref() != Some(LIBRARY)
                && !a
                    .warnings
                    .iter()
                    .any(|w| w.starts_with("openlibrary: not reached; showing what was kept"))
            {
                unsaid.push(format!("{}: {:?}", q.id, a.warnings));
            }
            offline.push((q.gates, score(q, &a.rows, &held_ids).0));
        }
        let f = gated(&offline);
        println!(
            "{:<14} {:>4}  {:>6.3}  {:>6.3}  {:>6.3}  open library unreachable",
            "offline", f.n, f.at1, f.top5, f.mrr
        );
        println!();

        assert_eq!(asked, 0, "the repeat run asked Open Library {asked} times");
        assert!(
            moved.is_empty(),
            "the repeat run answered differently: {moved:?}"
        );
        if !with_google {
            // Google is never kept, so its rows are missing offline by design; compare Open Library's half.
            assert!(
                differ.is_empty(),
                "offline answered differently from the first run: {differ:?}"
            );
        }
        assert!(
            unsaid.is_empty(),
            "an offline answer did not say it came from what was kept:\n  {}",
            unsaid.join("\n  ")
        );
    }

    let path = Path::new(BENCH_DIR).join("baseline.json");
    if env_on("UPDATE_BASELINE") {
        let baseline = Baseline {
            measured: "bench_search: Engine::search as a frontend's add-a-book box calls it (an ISBN typed into the same box), a search session run to done and flattened to each work's default edition, with the offline index's correction, notability and author names; you-already-have-it read off each work's Held (certain or likely)".into(),
            figures: current,
        };
        let text = serde_json::to_string_pretty(&baseline).expect("baseline serialises");
        std::fs::write(&path, text + "\n").expect("write baseline.json");
        println!("baseline.json rewritten");
        return;
    }
    let baseline: Baseline = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("baseline.json (UPDATE_BASELINE=1 writes it)"),
    )
    .expect("baseline.json parses");
    let mut drops = Vec::new();
    for (cat, was) in &baseline.figures {
        let Some(now) = current.get(cat) else {
            drops.push(format!("{cat}: in the baseline, not measured now"));
            continue;
        };
        for (name, b, c) in [
            ("@1", was.at1, now.at1),
            ("top5", was.top5, now.top5),
            ("MRR", was.mrr, now.mrr),
        ] {
            if round4(c) < round4(b) {
                drops.push(format!("{cat} {name}: {c:.4} < baseline {b:.4}"));
            }
        }
    }
    assert!(
        drops.is_empty(),
        "search got worse than baseline.json:\n  {}\n(a change that moves a figure on purpose commits UPDATE_BASELINE=1's rewrite)",
        drops.join("\n  ")
    );
}

#[cfg(test)]
mod scoring {
    use super::*;

    #[test]
    fn folding_ignores_case_marks_and_one_article() {
        assert_eq!(fold_title("The Hitchhiker's Guide"), "hitchhikers guide");
        assert_eq!(fold("Cien años de soledad"), "cien anos de soledad");
        assert_eq!(fold_title("À la recherche"), "a la recherche");
    }

    #[test]
    fn authors_match_across_initials_but_not_across_siblings() {
        let tolkien = vec!["J.R.R. Tolkien".to_string()];
        assert!(same_author(&tolkien, "John Ronald Reuel Tolkien"));
        assert!(same_author(&tolkien, "J. R. R. Tolkien"));
        let herbert = vec!["Frank Herbert".to_string()];
        assert!(!same_author(&herbert, "Brian Herbert"));
        let le_guin = vec!["Ursula K. Le Guin".to_string()];
        assert!(same_author(&le_guin, "Ursula K. LeGuin"));
    }
}
