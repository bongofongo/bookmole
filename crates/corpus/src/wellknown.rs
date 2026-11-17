//! `gen-wellknown`: pulls the offline index of well-known works from Wikidata (CC0); run by hand,
//! never by tests or CI. Queries are generic and the `User-Agent` is only `bookmole/<version>`.
//! Output: a gzip of text sorted by Q-number; `#` lines are comments, others are tab-separated
//! `qid sitelinks year ol_works author series series_pos title…`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use sha2::{Digest, Sha256};

/// Bumped when the file's layout changes; the engine refuses a version it does not know.
pub const FORMAT_VERSION: u32 = 1;

const ENDPOINT: &str = "https://query.wikidata.org/sparql";
const USER_AGENT: &str = concat!("bookmole/", env!("CARGO_PKG_VERSION"));

/// Direct instances only (`wdt:P31`): the `P31/P279*` closure times out. Series are left out on
/// purpose.
pub const CLASSES: &[(&str, &str)] = &[
    ("Q7725634", "literary work"),
    ("Q47461344", "written work"),
    ("Q8261", "novel"),
    ("Q571", "book"),
    ("Q49084", "short story"),
    ("Q149537", "novella"),
    ("Q1279564", "short story collection"),
    ("Q25379", "play"),
    ("Q5185279", "poem"),
    ("Q37484", "epic poem"),
    ("Q12106333", "poetry collection"),
    ("Q20540385", "non-fiction book"),
    ("Q725377", "graphic novel"),
];

pub const SINGLE_SITELINK_CLASSES: &[(&str, &str)] = &[
    ("Q7725634", "literary work"),
    ("Q47461344", "written work"),
    ("Q571", "book"),
];

const WIKIPEDIA_BATCH: usize = 2000;

/// A streamed answer cut at the service's one-minute limit can still arrive with a 200, so anything
/// this slow counts as cut off.
const CUT_OFF: Duration = Duration::from_secs(55);

pub const LANGS: &[&str] = &["en", "fr", "de", "es", "it", "pt", "nl", "sv", "pl", "mul"];

const MAX_ALIASES: usize = 4;

pub struct Options {
    pub scratch: PathBuf,
    pub out: PathBuf,
    pub target: usize,
    pub keep: Option<usize>,
    pub chunk: usize,
    pub min_sitelinks: u32,
    pub min_pause: Duration,
    pub assemble_only: bool,
    pub built: Option<String>,
}

pub fn default_out() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../engine/data")
}

pub fn default_scratch() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/wellknown-scratch")
}

pub struct Report {
    pub works: usize,
    pub bytes: u64,
    pub sha256: String,
    pub path: PathBuf,
}

#[expect(clippy::too_many_lines)]
pub fn generate(o: &Options) -> Result<Report, String> {
    std::fs::create_dir_all(&o.scratch).map_err(|e| format!("{}: {e}", o.scratch.display()))?;
    let mut client = if o.assemble_only {
        None
    } else {
        Some(Client::new(o.min_pause)?)
    };

    let mut best: HashMap<u64, u32> = HashMap::new();
    for (class, name) in CLASSES {
        let path = o.scratch.join(format!("ids-{class}.tsv"));
        if !path.exists() {
            let c = client.as_mut().ok_or_else(|| {
                format!("{} is missing; run without --assemble-only", path.display())
            })?;
            eprintln!("ids: {class} ({name})");
            let pairs = c.ids_of(class, o.min_sitelinks.max(2))?;
            write_pairs(&path, &pairs)?;
        }
        for (q, sl) in read_pairs(&path)? {
            let e = best.entry(q).or_insert(0);
            *e = (*e).max(sl);
        }
    }
    if o.min_sitelinks <= 1 {
        // One-sitelink works are kept only if they have a Wikipedia article (the rest are mostly Wikisource
        // pages). Asked in two steps, ids then articles: both at once outruns the service's minute.
        let mut singles: Vec<u64> = Vec::new();
        for (class, name) in SINGLE_SITELINK_CLASSES {
            for digit in 0..10 {
                let path = o.scratch.join(format!("one-{class}-{digit}.tsv"));
                if !path.exists() {
                    let c = client.as_mut().ok_or_else(|| {
                        format!("{} is missing; run without --assemble-only", path.display())
                    })?;
                    eprintln!("ids: {class} ({name}) with one sitelink, ending {digit}");
                    let ids = c.ids_with_one_sitelink(class, &digit.to_string())?;
                    write_pairs(&path, &ids.iter().map(|q| (*q, 1)).collect::<Vec<_>>())?;
                }
                singles.extend(read_pairs(&path)?.into_iter().map(|(q, _)| q));
            }
        }
        singles.sort_unstable();
        singles.dedup();
        singles.retain(|q| !best.contains_key(q));
        let dir = o.scratch.join("wikipedia");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let (asked, found) = read_batches(&dir)?;
        let pending: Vec<u64> = singles
            .iter()
            .copied()
            .filter(|q| !asked.contains(q))
            .collect();
        let batches: Vec<&[u64]> = pending.chunks(WIKIPEDIA_BATCH).collect();
        let mut with_article: Vec<u64> = found.into_iter().map(|r| r.item).collect();
        for (i, batch) in batches.iter().enumerate() {
            let c = client.as_mut().ok_or_else(|| {
                format!(
                    "{} works unchecked; run without --assemble-only",
                    pending.len()
                )
            })?;
            eprintln!("wikipedia articles: batch {} of {}", i + 1, batches.len());
            let yes = c.with_wikipedia_article(batch)?;
            let rows: Vec<Row> = yes
                .iter()
                .map(|q| Row {
                    item: *q,
                    kind: 'p',
                    v: String::new(),
                    x: String::new(),
                    y: String::new(),
                })
                .collect();
            write_batch(&dir.join(format!("Q{:012}.tsv", batch[0])), batch, &rows)?;
            with_article.extend(yes);
        }
        let singles: std::collections::HashSet<u64> = singles.into_iter().collect();
        for q in with_article {
            if singles.contains(&q) {
                best.entry(q).or_insert(1);
            }
        }
    }
    let mut selected: Vec<(u64, u32)> = best.into_iter().collect();
    selected.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    selected.truncate(o.target);
    selected.sort_by_key(|(q, _)| *q);
    eprintln!(
        "selected {} works (sitelinks >= {})",
        selected.len(),
        selected.iter().map(|(_, s)| *s).min().unwrap_or(0)
    );

    // Each batch file names the ids it answered on its first line, so a resume asks only for ids no
    // file holds.
    let dir = o.scratch.join("details");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (done, rows) = read_batches(&dir)?;
    let mut rows_of: HashMap<u64, Vec<Row>> = HashMap::new();
    for r in rows {
        rows_of.entry(r.item).or_default().push(r);
    }
    let pending: Vec<u64> = selected
        .iter()
        .map(|(q, _)| *q)
        .filter(|q| !done.contains(q))
        .collect();
    let batches: Vec<&[u64]> = pending.chunks(o.chunk.max(1)).collect();
    for (i, batch) in batches.iter().enumerate() {
        let c = client.as_mut().ok_or_else(|| {
            format!(
                "{} works have no details yet; run without --assemble-only",
                pending.len()
            )
        })?;
        eprintln!("details: batch {} of {}", i + 1, batches.len());
        let rows = c.details(batch)?;
        let path = dir.join(format!("Q{:012}.tsv", batch[0]));
        write_batch(&path, batch, &rows)?;
        for r in rows {
            rows_of.entry(r.item).or_default().push(r);
        }
    }

    let mut facts: BTreeMap<u64, Facts> = BTreeMap::new();
    for (q, sl) in &selected {
        let mut f = Facts {
            sitelinks: *sl,
            ..Default::default()
        };
        for r in rows_of.remove(q).unwrap_or_default() {
            f.add(r);
        }
        facts.insert(*q, f);
    }
    if let Some(keep) = o.keep.filter(|k| *k < facts.len()) {
        type Rank = (std::cmp::Reverse<u32>, std::cmp::Reverse<u8>, u64);
        let mut ranked: Vec<(u64, Rank)> = facts
            .iter()
            .map(|(q, f)| {
                let book_like = u8::from(!f.ol.is_empty())
                    + u8::from(!f.authors.is_empty())
                    + u8::from(!f.years.is_empty());
                (
                    *q,
                    (
                        std::cmp::Reverse(f.sitelinks),
                        std::cmp::Reverse(book_like),
                        *q,
                    ),
                )
            })
            .collect();
        ranked.sort_by_key(|(_, k)| *k);
        let kept: std::collections::HashSet<u64> =
            ranked.iter().take(keep).map(|(q, _)| *q).collect();
        facts.retain(|q, _| kept.contains(q));
    }
    let built = o.built.clone().unwrap_or_else(today_utc);
    let mut text = String::new();
    text.push_str(&format!("#readingbuddy-wellknown {FORMAT_VERSION}\n"));
    text.push_str("#source Wikidata (https://www.wikidata.org), CC0 1.0; query.wikidata.org\n");
    text.push_str(&format!("#built {built}\n"));
    text.push_str(
        "#columns qid sitelinks year ol_works author series series_pos titles([*~]langs=text)...\n",
    );
    let mut works = 0usize;
    for (q, f) in &facts {
        if let Some(line) = f.line(*q) {
            text.push_str(&line);
            text.push('\n');
            works += 1;
        }
    }
    std::fs::create_dir_all(&o.out).map_err(|e| format!("{}: {e}", o.out.display()))?;
    let path = o.out.join("wellknown.tsv.gz");
    let gz = gzip(text.as_bytes())?;
    std::fs::write(&path, &gz).map_err(|e| format!("{}: {e}", path.display()))?;
    let sha256 = hex(&Sha256::digest(&gz));
    let meta = format!(
        "{{\n  \"file\": \"wellknown.tsv.gz\",\n  \"format\": {FORMAT_VERSION},\n  \"sha256\": \"{sha256}\",\n  \"built\": \"{built}\",\n  \"works\": {works},\n  \"bytes\": {},\n  \"source\": \"Wikidata, CC0 1.0\"\n}}\n",
        gz.len()
    );
    let meta_path = o.out.join("wellknown.json");
    std::fs::write(&meta_path, meta).map_err(|e| format!("{}: {e}", meta_path.display()))?;
    Ok(Report {
        works,
        bytes: gz.len() as u64,
        sha256,
        path,
    })
}

struct Client {
    http: reqwest::blocking::Client,
    min_pause: Duration,
    last: Option<(Instant, Duration)>,
}

enum Answer {
    Ok(String),
    TooBig,
}

impl Client {
    fn new(min_pause: Duration) -> Result<Client, String> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(180))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Client {
            http,
            min_pause,
            last: None,
        })
    }

    fn pace(&mut self) {
        if let Some((at, took)) = self.last {
            let owed = self.min_pause.max(took);
            let since = at.elapsed();
            if since < owed {
                std::thread::sleep(owed - since);
            }
        }
    }

    /// POSTed because a batch of ids is too long for a URL.
    fn query(&mut self, sparql: &str, accept: &str) -> Result<Answer, String> {
        let mut backoff = Duration::from_secs(10);
        for attempt in 0..6 {
            self.pace();
            let start = Instant::now();
            let sent = self
                .http
                .post(ENDPOINT)
                .header("Accept", accept)
                .form(&[("query", sparql)])
                .send();
            let took = start.elapsed();
            self.last = Some((Instant::now(), took));
            let resp = match sent {
                Ok(r) => r,
                Err(e) if e.is_timeout() => {
                    eprintln!("  timed out after {took:?}");
                    return Ok(Answer::TooBig);
                }
                Err(e) => {
                    eprintln!("  attempt {attempt}: {e}; waiting {backoff:?}");
                    std::thread::sleep(backoff);
                    backoff *= 2;
                    continue;
                }
            };
            let status = resp.status().as_u16();
            if status == 429 || status == 503 {
                let wait = retry_after(resp.headers()).unwrap_or(backoff);
                eprintln!("  {status}: asked to wait {wait:?}");
                std::thread::sleep(wait + Duration::from_secs(1));
                backoff *= 2;
                continue;
            }
            let body = match resp.text() {
                Ok(b) => b,
                Err(e) if e.is_timeout() || e.is_body() || e.is_decode() => {
                    eprintln!("  the answer was cut off after {took:?}: {e}");
                    return Ok(Answer::TooBig);
                }
                Err(e) => return Err(e.to_string()),
            };
            let took = start.elapsed();
            self.last = Some((Instant::now(), took));
            if status == 200 && took >= CUT_OFF {
                eprintln!("  took {took:?}: treated as cut off at the service's minute");
                return Ok(Answer::TooBig);
            }
            if status == 200 {
                eprintln!("  ok in {took:?} ({} bytes)", body.len());
                return Ok(Answer::Ok(body));
            }
            if body.contains("TimeoutException") || body.contains("timeout") {
                eprintln!("  the service timed out after {took:?}");
                return Ok(Answer::TooBig);
            }
            if status >= 500 {
                eprintln!("  attempt {attempt}: status {status}; waiting {backoff:?}");
                std::thread::sleep(backoff);
                backoff *= 2;
                continue;
            }
            return Err(format!(
                "status {status}: {}",
                body.chars().take(400).collect::<String>()
            ));
        }
        Err("gave up after six attempts".into())
    }

    fn ids_of(&mut self, class: &str, min: u32) -> Result<Vec<(u64, u32)>, String> {
        let q = format!(
            "SELECT ?item ?sl WHERE {{ ?item wdt:P31 wd:{class} ; wikibase:sitelinks ?sl . FILTER(?sl >= {min}) }}"
        );
        match self.query(&q, TSV)? {
            Answer::Ok(body) => Ok(id_rows(&body)),
            Answer::TooBig => {
                if min >= 64 {
                    return Err(format!("{class}: times out even at >= {min} sitelinks"));
                }
                eprintln!("  {class}: too big at >= {min}; asking by bands");
                let mut out = self.ids_band(class, min, min * 2)?;
                out.extend(self.ids_of(class, min * 2)?);
                Ok(out)
            }
        }
    }

    fn ids_with_one_sitelink(&mut self, class: &str, suffix: &str) -> Result<Vec<u64>, String> {
        let q = format!(
            "SELECT ?item WHERE {{ ?item wdt:P31 wd:{class} ; wikibase:sitelinks 1 . FILTER(STRENDS(STR(?item), \"{suffix}\")) }}"
        );
        match self.query(&q, TSV)? {
            Answer::Ok(body) => Ok(body
                .lines()
                .skip(1)
                .filter_map(|l| qnum(l.trim().trim_start_matches('<').trim_end_matches('>')))
                .collect()),
            Answer::TooBig if suffix.len() < 4 => {
                eprintln!("  {class}: too big ending {suffix}; asking by longer endings");
                let mut out = Vec::new();
                for d in 0..10 {
                    out.extend(self.ids_with_one_sitelink(class, &format!("{d}{suffix}"))?);
                }
                Ok(out)
            }
            Answer::TooBig => Err(format!(
                "{class}: one-sitelink works ending {suffix} time out"
            )),
        }
    }

    /// Written in this order (`hint:optimizer "None"`): left to choose, the service starts from every
    /// Wikipedia article and times out.
    fn with_wikipedia_article(&mut self, ids: &[u64]) -> Result<Vec<u64>, String> {
        let values: Vec<String> = ids.iter().map(|q| format!("wd:Q{q}")).collect();
        let q = format!(
            "SELECT DISTINCT ?item WHERE {{ hint:Query hint:optimizer \"None\" . VALUES ?item {{ {} }} ?article schema:about ?item . ?article schema:isPartOf ?site . FILTER(CONTAINS(STR(?site), \".wikipedia.org\")) }}",
            values.join(" ")
        );
        match self.query(&q, TSV)? {
            Answer::Ok(body) => Ok(body
                .lines()
                .skip(1)
                .filter_map(|l| qnum(l.trim().trim_start_matches('<').trim_end_matches('>')))
                .collect()),
            Answer::TooBig if ids.len() > 1 => {
                let (a, b) = ids.split_at(ids.len() / 2);
                let mut out = self.with_wikipedia_article(a)?;
                out.extend(self.with_wikipedia_article(b)?);
                Ok(out)
            }
            Answer::TooBig => Err(format!("Q{}: one id times out", ids[0])),
        }
    }

    fn ids_band(&mut self, class: &str, lo: u32, hi: u32) -> Result<Vec<(u64, u32)>, String> {
        let q = format!(
            "SELECT ?item ?sl WHERE {{ ?item wdt:P31 wd:{class} ; wikibase:sitelinks ?sl . FILTER(?sl >= {lo} && ?sl < {hi}) }}"
        );
        match self.query(&q, TSV)? {
            Answer::Ok(body) => Ok(id_rows(&body)),
            Answer::TooBig => Err(format!("{class}: the band {lo}..{hi} times out")),
        }
    }

    fn details(&mut self, ids: &[u64]) -> Result<Vec<Row>, String> {
        match self.query(&details_query(ids), TSV)? {
            Answer::Ok(body) => parse_details_tsv(&body),
            Answer::TooBig if ids.len() > 1 => {
                let (a, b) = ids.split_at(ids.len() / 2);
                let mut out = self.details(a)?;
                out.extend(self.details(b)?);
                Ok(out)
            }
            Answer::TooBig => Err(format!("Q{}: one work times out", ids[0])),
        }
    }
}

const TSV: &str = "text/tab-separated-values";

fn id_rows(body: &str) -> Vec<(u64, u32)> {
    body.lines()
        .skip(1)
        .filter_map(|line| {
            let (item, sl) = line.split_once('\t')?;
            let q = qnum(item.trim().trim_start_matches('<').trim_end_matches('>'))?;
            Some((q, sl.trim().parse().ok()?))
        })
        .collect()
}

fn retry_after(h: &reqwest::header::HeaderMap) -> Option<Duration> {
    let v = h.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim();
    let secs = v.parse::<u64>().ok()?;
    Some(Duration::from_secs(secs.min(3600)))
}

fn details_query(ids: &[u64]) -> String {
    let values: Vec<String> = ids.iter().map(|q| format!("wd:Q{q}")).collect();
    let langs: Vec<String> = LANGS.iter().map(|l| format!("\"{l}\"")).collect();
    let langs = langs.join(",");
    // One branch per kind of fact, so a work with many authors and labels is a sum of rows, not a
    // product.
    format!(
        r#"SELECT ?item ?k ?v ?x ?y WHERE {{
  VALUES ?item {{ {values} }}
  {{ ?item rdfs:label ?v . FILTER(LANG(?v) IN ({langs})) BIND("l" AS ?k) BIND(LANG(?v) AS ?x) }}
  UNION {{ ?item skos:altLabel ?v . FILTER(LANG(?v) = "en") BIND("a" AS ?k) }}
  UNION {{ ?item wdt:P1476 ?v . BIND("t" AS ?k) BIND(LANG(?v) AS ?x) }}
  UNION {{ ?item p:P50 ?st . ?st ps:P50 ?a . BIND("w" AS ?k)
          OPTIONAL {{ ?st pq:P1545 ?x }}
          OPTIONAL {{ ?a rdfs:label ?en . FILTER(LANG(?en) = "en") }}
          OPTIONAL {{ ?a rdfs:label ?mul . FILTER(LANG(?mul) = "mul") }}
          OPTIONAL {{ ?a rdfs:label ?fr . FILTER(LANG(?fr) = "fr") }}
          OPTIONAL {{ ?a rdfs:label ?de . FILTER(LANG(?de) = "de") }}
          OPTIONAL {{ ?a rdfs:label ?es . FILTER(LANG(?es) = "es") }}
          BIND(COALESCE(?en, ?mul, ?fr, ?de, ?es) AS ?v)
          OPTIONAL {{ ?a wikibase:sitelinks ?y }} }}
  UNION {{ ?item wdt:P577 ?v . BIND("d" AS ?k) }}
  UNION {{ ?item p:P179 ?st . ?st ps:P179 ?s . BIND("s" AS ?k)
          OPTIONAL {{ ?st pq:P1545 ?x }}
          OPTIONAL {{ ?s rdfs:label ?sen . FILTER(LANG(?sen) = "en") }}
          OPTIONAL {{ ?s rdfs:label ?smul . FILTER(LANG(?smul) = "mul") }}
          BIND(COALESCE(?sen, ?smul) AS ?v) }}
  UNION {{ ?item wdt:P648 ?v . BIND("o" AS ?k) }}
}}"#,
        values = values.join(" ")
    )
}

fn qnum(uri: &str) -> Option<u64> {
    uri.rsplit('/').next()?.strip_prefix('Q')?.parse().ok()
}

#[derive(Debug, Clone, PartialEq)]
struct Row {
    item: u64,
    kind: char,
    v: String,
    x: String,
    y: String,
}

fn parse_details_tsv(body: &str) -> Result<Vec<Row>, String> {
    let mut lines = body.lines();
    let head = lines.next().unwrap_or_default();
    if head.split('\t').map(str::trim).collect::<Vec<_>>() != ["?item", "?k", "?v", "?x", "?y"] {
        return Err(format!("an answer with columns {head:?}"));
    }
    let mut out = Vec::new();
    for line in lines {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 5 {
            continue;
        }
        let item = f[0].trim().trim_start_matches('<').trim_end_matches('>');
        let (Some(item), Some(kind)) = (qnum(item), term(f[1]).chars().next()) else {
            continue;
        };
        out.push(Row {
            item,
            kind,
            v: term(f[2]),
            x: term(f[3]),
            y: term(f[4]),
        });
    }
    Ok(out)
}

fn term(t: &str) -> String {
    let t = t.trim();
    if let Some(iri) = t.strip_prefix('<') {
        return iri.trim_end_matches('>').to_string();
    }
    let Some(rest) = t.strip_prefix('"') else {
        return t.to_string();
    };
    let mut out = String::new();
    let mut it = rest.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => break,
            '\\' => match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('u') => {
                    let hex: String = it.by_ref().take(4).collect();
                    if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(c);
                    }
                }
                Some('U') => {
                    let hex: String = it.by_ref().take(8).collect();
                    if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(c);
                    }
                }
                Some(o) => out.push(o),
                None => {}
            },
            c => out.push(c),
        }
    }
    out
}

#[derive(Default)]
struct Facts {
    sitelinks: u32,
    labels: BTreeMap<String, String>,
    aliases: BTreeSet<String>,
    titles: BTreeSet<(String, String)>,
    /// (ordinal, author notability, name)
    authors: BTreeSet<(Option<u64>, u32, String)>,
    years: BTreeSet<i32>,
    /// (has ordinal, ordinal text, series name)
    series: BTreeSet<(bool, String, String)>,
    ol: BTreeSet<String>,
}

fn clean(s: &str) -> String {
    s.split(['\t', '\n', '\r'])
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl Facts {
    fn add(&mut self, r: Row) {
        let v = clean(&r.v);
        match r.kind {
            'l' if !v.is_empty() => {
                self.labels.insert(r.x.clone(), v);
            }
            'a' if !v.is_empty() => {
                self.aliases.insert(v);
            }
            't' if !v.is_empty() => {
                self.titles.insert((r.x.clone(), v));
            }
            'w' if !v.is_empty() => {
                let ord = r.x.trim().parse::<u64>().ok();
                let sl = r.y.parse::<u32>().unwrap_or(0);
                self.authors.insert((ord, sl, v));
            }
            'd' => {
                if let Some(y) = year_of(&r.v) {
                    self.years.insert(y);
                }
            }
            's' if !v.is_empty() => {
                let ord = clean(&r.x);
                self.series.insert((!ord.is_empty(), ord, v));
            }
            'o' => {
                let id = r.v.trim();
                if id.starts_with("OL")
                    && id.ends_with('W')
                    && id[2..id.len() - 1].bytes().all(|b| b.is_ascii_digit())
                {
                    self.ol.insert(id.to_string());
                }
            }
            _ => {}
        }
    }

    fn author(&self) -> Option<&str> {
        let numbered = self
            .authors
            .iter()
            .filter(|a| a.0.is_some())
            .min_by_key(|a| a.0);
        numbered
            .or_else(|| {
                self.authors
                    .iter()
                    .max_by(|a, b| a.1.cmp(&b.1).then(b.2.cmp(&a.2)))
            })
            .map(|a| a.2.as_str())
    }

    fn series(&self) -> Option<(&str, String)> {
        let s = self
            .series
            .iter()
            .rev()
            .find(|s| s.0)
            .or_else(|| self.series.iter().next())?;
        let pos =
            s.1.parse::<f64>()
                .ok()
                .filter(|p| p.is_finite() && *p >= 0.0 && *p < 10_000.0)
                .map(|p| {
                    if p.fract() == 0.0 {
                        format!("{}", p as i64)
                    } else {
                        format!("{p}")
                    }
                })
                .unwrap_or_default();
        Some((s.2.as_str(), pos))
    }

    fn line(&self, q: u64) -> Option<String> {
        let mut by_text: BTreeMap<&str, (BTreeSet<&str>, bool)> = BTreeMap::new();
        for (lang, text) in &self.labels {
            by_text
                .entry(text.as_str())
                .or_default()
                .0
                .insert(lang.as_str());
        }
        for (lang, text) in &self.titles {
            let e = by_text.entry(text.as_str()).or_default();
            if !lang.is_empty() {
                e.0.insert(lang.as_str());
            }
            e.1 = true;
        }
        if by_text.is_empty() {
            return None;
        }
        let mut fields: Vec<String> = by_text
            .iter()
            .map(|(text, (langs, original))| {
                let langs: Vec<&str> = langs.iter().copied().collect();
                format!(
                    "{}{}={}",
                    if *original { "*" } else { "" },
                    langs.join(","),
                    text
                )
            })
            .collect();
        let mut aliases: Vec<&String> = self
            .aliases
            .iter()
            .filter(|a| !by_text.contains_key(a.as_str()))
            .collect();
        aliases.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
        for a in aliases.into_iter().take(MAX_ALIASES) {
            fields.push(format!("~en={a}"));
        }
        let year = self
            .years
            .iter()
            .next()
            .map(|y| y.to_string())
            .unwrap_or_default();
        let ol: Vec<&str> = self.ol.iter().map(String::as_str).collect();
        let (series, pos) = self
            .series()
            .map(|(s, p)| (s.to_string(), p))
            .unwrap_or_default();
        Some(format!(
            "Q{q}\t{}\t{year}\t{}\t{}\t{series}\t{pos}\t{}",
            self.sitelinks,
            ol.join(","),
            self.author().unwrap_or(""),
            fields.join("\t")
        ))
    }
}

fn year_of(v: &str) -> Option<i32> {
    let v = v.trim();
    let (neg, rest) = match v.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let y: i32 = digits.parse().ok()?;
    if y == 0 || y > 3000 {
        return None;
    }
    Some(if neg { -y } else { y })
}

fn write_pairs(path: &Path, pairs: &[(u64, u32)]) -> Result<(), String> {
    let mut s = String::new();
    for (q, sl) in pairs {
        s.push_str(&format!("{q}\t{sl}\n"));
    }
    write_atomic(path, s.as_bytes())
}

fn read_pairs(path: &Path) -> Result<Vec<(u64, u32)>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for line in BufReader::new(f).lines() {
        let line = line.map_err(|e| e.to_string())?;
        let mut it = line.split('\t');
        if let (Some(q), Some(sl)) = (it.next(), it.next())
            && let (Ok(q), Ok(sl)) = (q.parse(), sl.parse())
        {
            out.push((q, sl));
        }
    }
    Ok(out)
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn unesc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn write_batch(path: &Path, ids: &[u64], rows: &[Row]) -> Result<(), String> {
    let ids: Vec<String> = ids.iter().map(u64::to_string).collect();
    let mut s = format!("#ids {}\n", ids.join(","));
    s.push_str(&rows_text(rows));
    write_atomic(path, s.as_bytes())
}

fn read_batches(dir: &Path) -> Result<(std::collections::HashSet<u64>, Vec<Row>), String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "tsv"))
        .collect();
    files.sort();
    let mut done = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for f in &files {
        let (ids, r) = read_batch(f)?;
        done.extend(ids);
        rows.extend(r);
    }
    Ok((done, rows))
}

fn read_batch(path: &Path) -> Result<(Vec<u64>, Vec<Row>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (head, body) = text.split_once('\n').unwrap_or((text.as_str(), ""));
    let ids = head
        .strip_prefix("#ids ")
        .ok_or_else(|| format!("{}: no #ids line", path.display()))?
        .split(',')
        .filter_map(|q| q.parse().ok())
        .collect();
    Ok((ids, rows_of_text(body)))
}

fn rows_text(rows: &[Row]) -> String {
    let mut s = String::new();
    for r in rows {
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            r.item,
            r.kind,
            esc(&r.v),
            esc(&r.x),
            esc(&r.y)
        ));
    }
    s
}

fn rows_of_text(text: &str) -> Vec<Row> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 5 {
            continue;
        }
        let (Ok(item), Some(kind)) = (f[0].parse(), f[1].chars().next()) else {
            continue;
        };
        out.push(Row {
            item,
            kind,
            v: unesc(f[2]),
            x: unesc(f[3]),
            y: unesc(f[4]),
        });
    }
    out
}

/// Written beside, then renamed: a killed run leaves no half file for a resume to take as finished.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("part");
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    // No name and a zero mtime in the header: the same text gives the same bytes and sha256.
    let mut enc = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::best());
    enc.write_all(bytes).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[allow(clippy::disallowed_methods)]
fn today_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: char, v: &str, x: &str, y: &str) -> Row {
        Row {
            item: 190192,
            kind,
            v: v.into(),
            x: x.into(),
            y: y.into(),
        }
    }

    #[test]
    fn a_work_is_one_sorted_line_with_shared_labels_folded_together() {
        let mut f = Facts {
            sitelinks: 77,
            ..Default::default()
        };
        for r in [
            row('l', "Dune", "en", ""),
            row('l', "Dune", "fr", ""),
            row('l', "Der Wüstenplanet", "de", ""),
            row('t', "Dune", "en", ""),
            row('a', "Dune (novel)", "", ""),
            row('w', "Frank Herbert", "", "120"),
            row('d', "1965-08-01T00:00:00Z", "", ""),
            row('d', "1966-01-01T00:00:00Z", "", ""),
            row('s', "Dune", "1", ""),
            row('o', "OL893414W", "", ""),
            row('o', "OL1234M", "", ""),
        ] {
            f.add(r);
        }
        assert_eq!(
            f.line(190192).unwrap(),
            "Q190192\t77\t1965\tOL893414W\tFrank Herbert\tDune\t1\tde=Der Wüstenplanet\t*en,fr=Dune\t~en=Dune (novel)"
        );
    }

    #[test]
    fn the_first_author_is_the_numbered_one_else_the_best_known() {
        let mut f = Facts::default();
        f.add(row('w', "B Second", "2", "900"));
        f.add(row('w', "A First", "1", "3"));
        assert_eq!(f.author(), Some("A First"));
        let mut g = Facts::default();
        g.add(row('w', "Lesser", "", "3"));
        g.add(row('w', "Greater", "", "50"));
        assert_eq!(g.author(), Some("Greater"));
    }

    #[test]
    fn years_are_read_with_their_sign() {
        assert_eq!(year_of("1965-08-01T00:00:00Z"), Some(1965));
        assert_eq!(year_of("-0800-01-01T00:00:00Z"), Some(-800));
        assert_eq!(year_of("+1605-01-01T00:00:00Z"), Some(1605));
        assert_eq!(year_of("garbage"), None);
    }

    #[test]
    fn scratch_rows_round_trip_whatever_they_hold() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.tsv");
        let rows = vec![row('l', "a\tb\\c\nd", "en", "")];
        write_batch(&p, &[190192, 74287], &rows).unwrap();
        assert_eq!(read_batch(&p).unwrap(), (vec![190192, 74287], rows));
    }

    #[test]
    fn details_read_the_service_s_tsv() {
        let body = "?item\t?k\t?v\t?x\t?y\n<http://www.wikidata.org/entity/Q190192>\t\"l\"\t\"Der W\\u00FCstenplanet \\\"x\\\"\"@de\t\"de\"\t\n<http://www.wikidata.org/entity/Q190192>\t\"w\"\t\"Frank Herbert\"@en\t\t\"120\"^^<http://www.w3.org/2001/XMLSchema#integer>\n";
        let rows = parse_details_tsv(body).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].v, "Der Wüstenplanet \"x\"");
        assert_eq!(rows[0].x, "de");
        assert_eq!(rows[1].kind, 'w');
        assert_eq!(rows[1].y, "120");
        assert!(parse_details_tsv("?a\n").is_err());
    }

    #[test]
    fn id_rows_read_the_service_s_tsv() {
        let body = "?item\t?sl\n<http://www.wikidata.org/entity/Q190192>\t77\n<http://www.wikidata.org/entity/Q74287>\t117\n";
        assert_eq!(id_rows(body), vec![(190192, 77), (74287, 117)]);
    }

    #[test]
    fn the_date_is_a_date() {
        let d = today_utc();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"));
    }
}
