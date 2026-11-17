//! The offline index of well-known works: suggestions and typo correction come from data on the
//! machine, so providers are asked only on Enter and never see a keystroke prefix.
//! Built from Wikidata alone and never updated from use: nothing a search does writes to it.

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

use super::query::{fold, is_filler, title_key};
use crate::diagnostic::ErrorClass;
use crate::error::{EngineError, Result};

/// The layout version this reader knows; another number is refused rather than guessed at.
pub const FORMAT_VERSION: u32 = 1;

/// Layout of the derived database, part of its file name; bump with any change to [`build`] or the
/// schema.
pub const INDEX_LAYOUT: u32 = 1;

#[cfg(feature = "wellknown")]
pub(crate) const EMBEDDED: Option<(&[u8], &str)> = Some((
    include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/data/wellknown.tsv.gz"
    )),
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/wellknown.json")),
));
#[cfg(not(feature = "wellknown"))]
pub(crate) const EMBEDDED: Option<(&[u8], &str)> = None;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct EmbeddedRecord {
    pub sha256: String,
    pub built: String,
    pub works: usize,
    pub bytes: usize,
}

pub fn embedded_record() -> Option<EmbeddedRecord> {
    EMBEDDED.and_then(|(_, meta)| serde_json::from_str(meta).ok())
}

pub fn embedded_bytes() -> Option<&'static [u8]> {
    EMBEDDED.map(|(b, _)| b)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WellKnownWork {
    /// The Wikidata id's number (`Q190192` is 190192).
    pub wikidata: u64,
    /// Notability: how many Wikimedia sites have a page about it.
    pub sitelinks: u32,
    pub year: Option<i32>,
    /// Open Library work ids, bare (`OL893414W`).
    pub ol_works: Vec<String>,
    pub author: Option<String>,
    pub series: Option<String>,
    pub series_position: Option<f64>,
    pub titles: Vec<WellKnownTitle>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WellKnownTitle {
    pub text: String,
    /// The languages whose label this is (two-letter, or `mul`).
    pub langs: Vec<String>,
    pub original: bool,
    pub alias: bool,
}

impl WellKnownWork {
    pub fn display_title(&self) -> &str {
        let pick = |f: &dyn Fn(&WellKnownTitle) -> bool| self.titles.iter().find(|t| f(t));
        pick(&|t| !t.alias && t.langs.iter().any(|l| l == "en"))
            .or_else(|| pick(&|t| t.original))
            .or_else(|| pick(&|t| !t.alias && t.langs.iter().any(|l| l == "mul")))
            .or_else(|| self.titles.first())
            .map(|t| t.text.as_str())
            .unwrap_or("")
    }
}

/// Comment lines are skipped; the first must name a format this reader knows.
pub fn parse_works(text: &str) -> Result<Vec<WellKnownWork>> {
    let mut lines = text.lines();
    let head = lines.next().unwrap_or("");
    let version = head
        .strip_prefix("#readingbuddy-wellknown ")
        .and_then(|v| v.trim().parse::<u32>().ok());
    if version != Some(FORMAT_VERSION) {
        return Err(EngineError::InvalidInput(format!(
            "not a well-known works file of format {FORMAT_VERSION}"
        )));
    }
    let mut out = Vec::new();
    for line in lines {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(w) = parse_line(line) {
            out.push(w);
        }
    }
    Ok(out)
}

fn parse_line(line: &str) -> Option<WellKnownWork> {
    let mut f = line.split('\t');
    let wikidata = f.next()?.strip_prefix('Q')?.parse().ok()?;
    let sitelinks = f.next()?.parse().ok()?;
    let year = f.next()?.parse().ok();
    let ol_works = f
        .next()?
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let nonempty = |s: &str| (!s.is_empty()).then(|| s.to_string());
    let author = nonempty(f.next()?);
    let series = nonempty(f.next()?);
    let series_position = f.next()?.parse().ok();
    let titles: Vec<WellKnownTitle> = f
        .filter_map(|field| {
            let (head, text) = field.split_once('=')?;
            let original = head.starts_with('*');
            let alias = head.starts_with('~');
            let langs = head
                .trim_start_matches(['*', '~'])
                .split(',')
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            (!text.is_empty()).then(|| WellKnownTitle {
                text: text.to_string(),
                langs,
                original,
                alias,
            })
        })
        .collect();
    (!titles.is_empty()).then_some(WellKnownWork {
        wikidata,
        sitelinks,
        year,
        ol_works,
        author,
        series,
        series_position,
        titles,
    })
}

pub fn gunzip(bytes: &[u8]) -> Result<String> {
    let mut text = String::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_string(&mut text)
        .map_err(EngineError::Io)?;
    Ok(text)
}

#[derive(Debug, Clone)]
pub enum Loader {
    Embedded { dir: PathBuf },
    Works(Arc<Vec<WellKnownWork>>),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    NotInThisBuild,
    NotConfigured,
    Failed(ErrorClass),
}

#[derive(Debug, Clone)]
pub struct WellKnown {
    pool: SqlitePool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WellKnownHit {
    pub wikidata: u64,
    pub title: String,
    pub titles: Vec<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    pub ol_works: Vec<String>,
    pub series: Option<String>,
    pub series_position: Option<f64>,
    pub sitelinks: u32,
}

impl WellKnown {
    pub async fn open(loader: &Loader) -> std::result::Result<WellKnown, Unavailable> {
        match loader {
            Loader::None => Err(Unavailable::NotConfigured),
            Loader::Works(works) => Self::in_memory(works)
                .await
                .map_err(|e| Unavailable::Failed(ErrorClass::from(&e))),
            Loader::Embedded { dir } => {
                let Some((bytes, _)) = EMBEDDED else {
                    return Err(Unavailable::NotInThisBuild);
                };
                let Some(record) = embedded_record() else {
                    return Err(Unavailable::Failed(ErrorClass::Parse));
                };
                Self::on_disk(dir, &record, bytes).await.map_err(|e| {
                    tracing::warn!(class = ?ErrorClass::from(&e), "the offline index could not be opened");
                    Unavailable::Failed(ErrorClass::from(&e))
                })
            }
        }
    }

    /// One connection: an in-memory database is per connection.
    pub async fn in_memory(works: &[WellKnownWork]) -> Result<WellKnown> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(SqliteConnectOptions::new().in_memory(true))
            .await?;
        build(&pool, works, "memory").await?;
        Ok(WellKnown { pool })
    }

    pub async fn on_disk(dir: &Path, record: &EmbeddedRecord, bytes: &[u8]) -> Result<WellKnown> {
        // The file hash and layout form the stamp: a new data file or layout is a new derived file, never a
        // stale one reused.
        let stamp = format!(
            "{}-v{INDEX_LAYOUT}",
            record.sha256.chars().take(16).collect::<String>()
        );
        let path = dir.join(format!("wellknown-{stamp}.db"));
        if path.exists() {
            match Self::open_file(&path, &stamp).await {
                Ok(w) => return Ok(w),
                Err(e) => {
                    tracing::info!(class = ?ErrorClass::from(&e), "the offline index is not whole; building it again");
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        build_file(dir, &path, &stamp, bytes).await?;
        Self::open_file(&path, &stamp).await
    }

    async fn open_file(path: &Path, stamp: &str) -> Result<WellKnown> {
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .read_only(true)
                    .create_if_missing(false),
            )
            .await?;
        let complete: Option<String> =
            sqlx::query_scalar("SELECT value FROM meta WHERE key = 'stamp'")
                .fetch_optional(&pool)
                .await?;
        if complete.as_deref() != Some(stamp) {
            pool.close().await;
            return Err(EngineError::InvalidInput(
                "the offline index is from another build".into(),
            ));
        }
        Ok(WellKnown { pool })
    }

    pub async fn count(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT count(*) FROM works")
            .fetch_one(&self.pool)
            .await?)
    }

    /// Works whose title (with or without its leading article) or author begins with `prefix`, best
    /// known first.
    pub async fn suggest(&self, prefix: &str, limit: usize) -> Result<Vec<WellKnownHit>> {
        let p = fold(prefix);
        if p.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        // Titles are kept by key (`query::title_key`), so `the hob` and `hob` both reach *The Hobbit*.
        let key = title_key(&p).to_string();
        let hi = format!("{p}\u{10FFFF}");
        let key_hi = format!("{key}\u{10FFFF}");
        let lim = (limit * 4) as i64;
        // A short prefix matches many titles: walk them best known first and stop at the limit.
        let sql = if key.chars().count() <= SHORT_PREFIX {
            "SELECT work, text, sitelinks FROM titles INDEXED BY titles_rank
             WHERE key >= ?1 AND key < ?2 ORDER BY sitelinks DESC, work LIMIT ?3"
        } else {
            "SELECT work, text, sitelinks FROM titles INDEXED BY titles_key
             WHERE key >= ?1 AND key < ?2 ORDER BY sitelinks DESC, work LIMIT ?3"
        };
        let rows = sqlx::query(sql)
            .bind(&key)
            .bind(&key_hi)
            .bind(lim)
            .fetch_all(&self.pool)
            .await?;
        let mut order: Vec<(u64, Option<String>)> = Vec::new();
        for r in rows {
            let work: i64 = r.get(0);
            let work = work as u64;
            if !order.iter().any(|(w, _)| *w == work) {
                order.push((work, Some(r.get::<String, _>(1))));
            }
        }
        if order.len() < limit && !p.contains(' ') && p.chars().count() >= 3 {
            let rows = sqlx::query(
                "SELECT a.work FROM author_words a JOIN works w ON w.id = a.work
                 WHERE a.word >= ?1 AND a.word < ?2
                 ORDER BY w.sitelinks DESC, a.work LIMIT ?3",
            )
            .bind(&p)
            .bind(&hi)
            .bind(lim)
            .fetch_all(&self.pool)
            .await?;
            for r in rows {
                let work = r.get::<i64, _>(0) as u64;
                if !order.iter().any(|(w, _)| *w == work) {
                    order.push((work, None));
                }
            }
        }
        order.truncate(limit);
        self.hits(&order).await
    }

    pub async fn candidates(&self, words: &[String], limit: usize) -> Result<Vec<WellKnownHit>> {
        let mut grams: BTreeSet<String> = BTreeSet::new();
        for w in words {
            if is_filler(w) {
                continue;
            }
            let chars: Vec<char> = w.chars().collect();
            for win in chars.windows(3) {
                grams.insert(win.iter().collect());
            }
        }
        if grams.is_empty() {
            return Ok(Vec::new());
        }
        // The rarest grams are asked: a common one (`the`, `her`) says little and costs most.
        let mut rated: Vec<(i64, String)> = Vec::new();
        for g in &grams {
            let doc: Option<i64> = sqlx::query_scalar("SELECT doc FROM grams_vocab WHERE term = ?")
                .bind(g)
                .fetch_optional(&self.pool)
                .await?;
            if let Some(d) = doc.filter(|d| *d > 0 && *d <= MAX_GRAM_DOCS) {
                rated.push((d, g.clone()));
            }
        }
        rated.sort();
        rated.truncate(MAX_GRAMS);
        // Ties go to the lower rowid: the build numbers titles by notability.
        if rated.is_empty() {
            return Ok(Vec::new());
        }
        let union = rated
            .iter()
            .map(|_| "SELECT rowid AS id FROM grams WHERE grams MATCH ?")
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        let sql =
            format!("SELECT id FROM ({union}) GROUP BY id ORDER BY count(*) DESC, id LIMIT ?");
        let mut query = sqlx::query_scalar::<_, i64>(&sql);
        for (_, g) in &rated {
            query = query.bind(format!("\"{}\"", g.replace('"', "\"\"")));
        }
        let best: Vec<i64> = query
            .bind((limit.saturating_mul(2)) as i64)
            .fetch_all(&self.pool)
            .await?;
        let ids: Vec<i64> = best;
        let rows = sqlx::query(
            "SELECT t.id, t.work FROM titles t WHERE t.id IN (SELECT value FROM json_each(?))",
        )
        .bind(serde_json::to_string(&ids).unwrap_or_default())
        .fetch_all(&self.pool)
        .await?;
        let work_of: HashMap<i64, u64> = rows
            .iter()
            .map(|r| (r.get::<i64, _>(0), r.get::<i64, _>(1) as u64))
            .collect();
        let mut order: Vec<(u64, Option<String>)> = Vec::new();
        for id in ids {
            if let Some(w) = work_of.get(&id)
                && !order.iter().any(|(x, _)| x == w)
            {
                order.push((*w, None));
            }
        }
        order.truncate(limit);
        self.hits(&order).await
    }

    /// A record is an index work when one of the work's titles is the record's and either the work
    /// names the record's Open Library id or its author shares the record's surname. The id alone is
    /// not trusted (Wikidata lists one work's id on another) and many works have none, so the author
    /// stands in.
    pub async fn notability(
        &self,
        records: &[(String, String, Option<String>)],
    ) -> Result<HashMap<String, u32>> {
        let probes: Vec<(String, String, String, String)> = records
            .iter()
            .filter_map(|(key, title, author)| {
                let bare = key.trim_start_matches("/works/").to_string();
                let folded = fold(title.split([':', '(']).next().unwrap_or(title));
                let k = title_key(&folded).to_string();
                let surname = author
                    .as_deref()
                    .map(fold)
                    .and_then(|a| a.split_whitespace().last().map(str::to_string))
                    .unwrap_or_default();
                (!k.is_empty()).then(|| (key.clone(), bare, k, surname))
            })
            .collect();
        if probes.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(
            "SELECT j.value->>0, max(w.sitelinks) FROM json_each(?) j
             JOIN titles t ON t.key = j.value->>2
             JOIN works w ON w.id = t.work
             WHERE EXISTS (SELECT 1 FROM ol o WHERE o.work = w.id AND o.ol = j.value->>1)
                OR EXISTS (SELECT 1 FROM author_words a WHERE a.work = w.id AND a.word = j.value->>3)
             GROUP BY j.value->>0",
        )
        .bind(serde_json::to_string(&probes).unwrap_or_default())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let sl: i64 = r.get(1);
                (r.get::<String, _>(0), u32::try_from(sl).unwrap_or(u32::MAX))
            })
            .collect())
    }

    pub async fn authors_named(&self, words: &[String]) -> Result<Vec<String>> {
        let ws: Vec<&str> = words
            .iter()
            .map(String::as_str)
            .filter(|w| w.chars().count() >= 3 && !is_filler(w))
            .collect();
        if ws.is_empty() {
            return Ok(Vec::new());
        }
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT w.author FROM author_words a JOIN works w ON w.id = a.work
             WHERE a.word IN (SELECT value FROM json_each(?)) AND w.author IS NOT NULL
             LIMIT 400",
        )
        .bind(serde_json::to_string(&ws).unwrap_or_default())
        .fetch_all(&self.pool)
        .await?;
        Ok(names)
    }

    async fn hits(&self, order: &[(u64, Option<String>)]) -> Result<Vec<WellKnownHit>> {
        if order.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<i64> = order.iter().map(|(w, _)| *w as i64).collect();
        let json = serde_json::to_string(&ids).unwrap_or_default();
        let works = sqlx::query(
            "SELECT id, sitelinks, year, ol, author, series, series_pos FROM works
             WHERE id IN (SELECT value FROM json_each(?))",
        )
        .bind(&json)
        .fetch_all(&self.pool)
        .await?;
        let titles = sqlx::query(
            "SELECT work, text, display FROM titles WHERE work IN (SELECT value FROM json_each(?))
             ORDER BY work, id",
        )
        .bind(&json)
        .fetch_all(&self.pool)
        .await?;
        let mut titles_of: HashMap<i64, (Vec<String>, Option<String>)> = HashMap::new();
        for t in titles {
            let e = titles_of.entry(t.get(0)).or_default();
            let text: String = t.get(1);
            if t.get::<i64, _>(2) == 1 && e.1.is_none() {
                e.1 = Some(text.clone());
            }
            if !e.0.contains(&text) {
                e.0.push(text);
            }
        }
        let mut by_id: HashMap<i64, WellKnownHit> = HashMap::new();
        for r in works {
            let id: i64 = r.get(0);
            let (titles, display) = titles_of.remove(&id).unwrap_or_default();
            let ol: Option<String> = r.get(3);
            by_id.insert(
                id,
                WellKnownHit {
                    wikidata: id as u64,
                    title: display
                        .or_else(|| titles.first().cloned())
                        .unwrap_or_default(),
                    titles,
                    author: r.get(4),
                    year: r.get::<Option<i64>, _>(2).map(|y| y as i32),
                    ol_works: ol
                        .map(|s| s.split(',').map(str::to_string).collect())
                        .unwrap_or_default(),
                    series: r.get(5),
                    series_position: r.get(6),
                    sitelinks: u32::try_from(r.get::<i64, _>(1)).unwrap_or(0),
                },
            );
        }
        Ok(order
            .iter()
            .filter_map(|(w, matched)| {
                let mut h = by_id.remove(&(*w as i64))?;
                if let Some(m) = matched {
                    h.title = m.clone();
                }
                Some(h)
            })
            .collect())
    }
}

const SHORT_PREFIX: usize = 2;

const MAX_GRAM_DOCS: i64 = 15_000;
const MAX_GRAMS: usize = 12;

const SCHEMA: &[&str] = &[
    "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    "CREATE TABLE works (
        id INTEGER PRIMARY KEY, sitelinks INTEGER NOT NULL, year INTEGER, ol TEXT,
        author TEXT, series TEXT, series_pos REAL)",
    "CREATE TABLE titles (
        id INTEGER PRIMARY KEY, work INTEGER NOT NULL, text TEXT NOT NULL,
        key TEXT NOT NULL, display INTEGER NOT NULL, sitelinks INTEGER NOT NULL)",
    "CREATE TABLE ol (ol TEXT NOT NULL, work INTEGER NOT NULL, PRIMARY KEY (ol, work)) WITHOUT ROWID",
    "CREATE TABLE author_words (word TEXT NOT NULL, work INTEGER NOT NULL, PRIMARY KEY (word, work)) WITHOUT ROWID",
    // Contentless (the text is in `titles`) and positionless (only membership is asked).
    "CREATE VIRTUAL TABLE grams USING fts5(text, content='', columnsize=0, detail=none, tokenize='trigram')",
    "CREATE VIRTUAL TABLE grams_vocab USING fts5vocab(grams, 'row')",
];

const INDEXES: &[&str] = &[
    "CREATE INDEX titles_key ON titles (key, sitelinks)",
    "CREATE INDEX titles_rank ON titles (sitelinks DESC, work, key)",
    "CREATE INDEX titles_work ON titles (work)",
];

const BATCH: usize = 4000;

async fn build_file(dir: &Path, path: &Path, stamp: &str, bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let started = std::time::Instant::now();
    let works = parse_works(&gunzip(bytes)?)?;
    // Built beside, then renamed: concurrent builders each write their own file and a half-built one is
    // never at the name.
    let part = dir.join(format!("wellknown-{stamp}.{}.part", std::process::id()));
    let _ = std::fs::remove_file(&part);
    {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&part)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Off)
                    .synchronous(SqliteSynchronous::Off),
            )
            .await?;
        build(&pool, &works, stamp).await?;
        pool.close().await;
    }
    std::fs::rename(&part, path)?;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("wellknown-") && name.ends_with(".db") && e.path() != path {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    tracing::info!(
        works = works.len(),
        millis = started.elapsed().as_millis() as u64,
        "offline index built"
    );
    Ok(())
}

/// The stamp is written last, so a file with a stamp is whole.
async fn build(pool: &SqlitePool, works: &[WellKnownWork], stamp: &str) -> Result<()> {
    let mut tx = pool.begin().await?;
    for s in SCHEMA {
        sqlx::query(s).execute(&mut *tx).await?;
    }
    let mut work_rows: Vec<serde_json::Value> = Vec::new();
    let mut title_rows: Vec<serde_json::Value> = Vec::new();
    let mut gram_rows: Vec<serde_json::Value> = Vec::new();
    let mut ol_rows: Vec<serde_json::Value> = Vec::new();
    let mut word_rows: Vec<serde_json::Value> = Vec::new();
    let mut next_title: i64 = 1;
    // Titles are numbered best known first, so a lower rowid is the more notable work where two titles
    // tie.
    let mut order: Vec<&WellKnownWork> = works.iter().collect();
    order.sort_by(|a, b| {
        b.sitelinks
            .cmp(&a.sitelinks)
            .then(a.wikidata.cmp(&b.wikidata))
    });
    for w in order {
        let id = w.wikidata as i64;
        let author_folded = w.author.as_deref().map(fold).unwrap_or_default();
        work_rows.push(serde_json::json!([
            id,
            w.sitelinks,
            w.year,
            (!w.ol_works.is_empty()).then(|| w.ol_works.join(",")),
            w.author,
            w.series,
            w.series_position,
        ]));
        for ol in &w.ol_works {
            ol_rows.push(serde_json::json!([ol, id]));
        }
        let mut words: BTreeSet<&str> = BTreeSet::new();
        for word in author_folded.split_whitespace() {
            if word.chars().count() >= 2 {
                words.insert(word);
            }
        }
        for word in words {
            word_rows.push(serde_json::json!([word, id]));
        }
        let display = w.display_title().to_string();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut shown = false;
        for t in &w.titles {
            let folded = fold(&t.text);
            if folded.is_empty() || !seen.insert(folded.clone()) {
                continue;
            }
            let is_display = !shown && t.text == display;
            shown |= is_display;
            let key = title_key(&folded).to_string();
            title_rows.push(serde_json::json!([
                next_title,
                id,
                t.text,
                key,
                i64::from(is_display),
                w.sitelinks,
            ]));
            gram_rows.push(serde_json::json!([
                next_title,
                format!("{folded} {author_folded}")
            ]));
            next_title += 1;
        }
    }
    insert(
        &mut tx,
        "INSERT INTO works (id, sitelinks, year, ol, author, series, series_pos)
         SELECT value->>0, value->>1, value->>2, value->>3, value->>4, value->>5, value->>6 FROM json_each(?)",
        &work_rows,
    )
    .await?;
    insert(
        &mut tx,
        "INSERT INTO titles (id, work, text, key, display, sitelinks)
         SELECT value->>0, value->>1, value->>2, value->>3, value->>4, value->>5 FROM json_each(?)",
        &title_rows,
    )
    .await?;
    insert(
        &mut tx,
        "INSERT OR IGNORE INTO ol (ol, work) SELECT value->>0, value->>1 FROM json_each(?)",
        &ol_rows,
    )
    .await?;
    insert(
        &mut tx,
        "INSERT OR IGNORE INTO author_words (word, work) SELECT value->>0, value->>1 FROM json_each(?)",
        &word_rows,
    )
    .await?;
    insert(
        &mut tx,
        "INSERT INTO grams (rowid, text) SELECT value->>0, value->>1 FROM json_each(?)",
        &gram_rows,
    )
    .await?;
    for s in INDEXES {
        sqlx::query(s).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO grams (grams) VALUES ('optimize')")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO meta (key, value) VALUES ('works', ?), ('stamp', ?)")
        .bind(works.len().to_string())
        .bind(stamp)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn insert(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    sql: &str,
    rows: &[serde_json::Value],
) -> Result<()> {
    for chunk in rows.chunks(BATCH) {
        let json = serde_json::to_string(chunk).map_err(|e| EngineError::Other(e.to_string()))?;
        sqlx::query(sql).bind(json).execute(&mut **tx).await?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod fixture {
    use super::*;

    pub(crate) fn work(
        q: u64,
        sitelinks: u32,
        title: &str,
        author: &str,
        ol: &str,
        year: i32,
    ) -> WellKnownWork {
        WellKnownWork {
            wikidata: q,
            sitelinks,
            year: Some(year),
            ol_works: if ol.is_empty() {
                Vec::new()
            } else {
                vec![ol.to_string()]
            },
            author: Some(author.to_string()),
            series: None,
            series_position: None,
            titles: vec![WellKnownTitle {
                text: title.to_string(),
                langs: vec!["en".into()],
                original: true,
                alias: false,
            }],
        }
    }

    pub(crate) fn works() -> Vec<WellKnownWork> {
        let mut dune = work(190192, 77, "Dune", "Frank Herbert", "OL893414W", 1965);
        dune.titles.push(WellKnownTitle {
            text: "Der Wüstenplanet".into(),
            langs: vec!["de".into()],
            original: false,
            alias: false,
        });
        let mut messiah = work(
            1199688,
            40,
            "Dune Messiah",
            "Frank Herbert",
            "OL893461W",
            1969,
        );
        messiah.series = Some("Dune".into());
        messiah.series_position = Some(2.0);
        vec![
            dune,
            messiah,
            work(
                74287,
                117,
                "The Hobbit",
                "J. R. R. Tolkien",
                "OL27482W",
                1937,
            ),
            work(
                170583,
                90,
                "Pride and Prejudice",
                "Jane Austen",
                "OL66554W",
                1813,
            ),
            work(
                180736,
                80,
                "Jane Eyre",
                "Charlotte Brontë",
                "OL1095427W",
                1847,
            ),
            work(
                161531,
                70,
                "Neuromancer",
                "William Gibson",
                "OL27258W",
                1984,
            ),
            work(
                150827,
                85,
                "Frankenstein",
                "Mary Shelley",
                "OL450063W",
                1818,
            ),
            work(167297, 75, "The Road", "Cormac McCarthy", "OL40873W", 2006),
            work(99999001, 3, "The Habit", "Someone Else", "", 2001),
        ]
    }

    pub(crate) async fn index() -> WellKnown {
        WellKnown::in_memory(&works())
            .await
            .expect("a fixture index builds")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_reads_back_as_the_generator_describes_it() {
        let text = "#readingbuddy-wellknown 1\n#built 2026-11-12\nQ190192\t77\t1965\tOL893414W\tFrank Herbert\tDune\t1\tde=Der Wüstenplanet\t*en,fr=Dune\t~en=Dune (novel)\n";
        let works = parse_works(text).unwrap();
        assert_eq!(works.len(), 1);
        let w = &works[0];
        assert_eq!(w.wikidata, 190192);
        assert_eq!(w.year, Some(1965));
        assert_eq!(w.ol_works, vec!["OL893414W".to_string()]);
        assert_eq!(w.series.as_deref(), Some("Dune"));
        assert_eq!(w.series_position, Some(1.0));
        assert_eq!(w.titles.len(), 3);
        assert!(w.titles[1].original);
        assert!(w.titles[2].alias);
        assert_eq!(w.display_title(), "Dune");
    }

    #[test]
    fn another_format_is_refused() {
        assert!(parse_works("#readingbuddy-wellknown 99\n").is_err());
        assert!(parse_works("Q1\t2\n").is_err());
    }

    #[tokio::test]
    async fn suggestions_are_prefixes_of_titles_or_authors_best_known_first() {
        let wk = fixture::index().await;
        let s = wk.suggest("dun", 5).await.unwrap();
        let titles: Vec<&str> = s.iter().map(|h| h.title.as_str()).collect();
        assert_eq!(titles, vec!["Dune", "Dune Messiah"]);
        assert_eq!(wk.suggest("hobb", 5).await.unwrap()[0].title, "The Hobbit");
        assert_eq!(
            wk.suggest("der wus", 5).await.unwrap()[0].title,
            "Der Wüstenplanet"
        );
        let by = wk.suggest("herbert", 5).await.unwrap();
        assert_eq!(by[0].title, "Dune");
        assert!(wk.suggest("", 5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn candidates_survive_a_typo_in_every_word() {
        let wk = fixture::index().await;
        let words = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        let c = wk.candidates(&words("dnue herbrt"), 10).await.unwrap();
        assert!(c.iter().any(|h| h.title == "Dune"), "{c:?}");
        let c = wk.candidates(&words("frankenstien"), 10).await.unwrap();
        assert_eq!(c[0].title, "Frankenstein");
    }

    #[tokio::test]
    async fn notability_is_read_by_open_library_id() {
        let wk = fixture::index().await;
        let n = wk
            .notability(&[
                ("/works/OL893414W".into(), "Dune".into(), None),
                ("/works/OL1W".into(), "Dune".into(), None),
                (
                    "/works/OL2W".into(),
                    "Dune".into(),
                    Some("Brian Herbert".into()),
                ),
                (
                    "/works/OL3W".into(),
                    "Dune".into(),
                    Some("Someone Else".into()),
                ),
            ])
            .await
            .unwrap();
        assert_eq!(n.get("/works/OL893414W"), Some(&77), "by its id and title");
        assert_eq!(n.get("/works/OL2W"), Some(&77), "by its title and surname");
        assert_eq!(n.len(), 2);
        let n = wk
            .notability(&[("/works/OL893414W".into(), "Dune Messiah".into(), None)])
            .await
            .unwrap();
        assert!(n.is_empty());
    }

    #[tokio::test]
    async fn authors_are_known_by_any_word_of_their_name() {
        let wk = fixture::index().await;
        let names = wk.authors_named(&["mccarthy".into()]).await.unwrap();
        assert_eq!(names, vec!["Cormac McCarthy".to_string()]);
        assert!(wk.authors_named(&["the".into()]).await.unwrap().is_empty());
    }
}
