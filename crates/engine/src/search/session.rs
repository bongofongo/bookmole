//! A search session: results arrive progressively and the caller polls for the next snapshot.
//! Each task is owned by the session and aborted on drop; nothing starts until a caller asks.
//! Google Books answers are never kept: they live in this session's memory only.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::correct::{self, Fix, Target, Verdict};
use super::held::{Held, Library};
use super::kept;
use super::parsed::{self, Plan, ResultNames};
use super::query::{KnowsNothing, ParsedQuery, ReadingKind, fold, parse, title_key};
use super::ranking::{self, Candidate};
use super::wellknown::{Unavailable, WellKnown};
use super::works::{DefaultContext, WorkResult, works};
use crate::book::Book;
use crate::diagnostic::{Diagnostic, DiagnosticKind, ErrorClass};
use crate::error::{EngineError, Result};
use crate::providers::{
    KeptRecord, MetadataProvider, ProviderAvailability, ProviderId, ProviderWork,
};
use crate::storage::{BookFilter, KeptEntry, KeptRow, LibraryScope, Storage, now_unix};

/// Latest the catalogue section is released, whether or not Open Library has answered.
pub const RELEASE_AFTER: Duration = Duration::from_millis(1200);

pub const MAX_WAIT: Duration = Duration::from_millis(1500);

pub const SESSION_IDLE: Duration = Duration::from_secs(10 * 60);

pub const MAX_SESSIONS: usize = 8;

/// Seconds a kept answer is used without asking the provider; older is shown, then refreshed.
pub const FRESH_FOR: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    Everything,
    LocalOnly,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderState {
    Off,
    NoKey,
    /// `kept` is when what is shown for it was fetched, if anything.
    NotAsked {
        kept: Option<i64>,
    },
    Asking {
        kept: Option<i64>,
    },
    Answered,
    FromCache {
        fetched_at: i64,
    },
    RateLimited {
        retry_after: Duration,
        kept: Option<i64>,
    },
    NotReached {
        diagnostic: Diagnostic,
        kept: Option<i64>,
    },
    Failed {
        diagnostic: Diagnostic,
        kept: Option<i64>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderStatus {
    pub provider: ProviderId,
    pub state: ProviderState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryReading {
    pub readings: Vec<ReadingKind>,
    pub isbn: Option<String>,
    /// Shaped like an ISBN and failing its checksum: searched as text.
    pub not_an_isbn: bool,
}

#[derive(Debug, Clone)]
pub struct HeldRow {
    pub book: Book,
    /// In the library by the copies rule.
    pub in_library: bool,
    pub has_copy: bool,
    pub passages: i64,
    pub notes: i64,
    pub lists: Vec<HeldList>,
    /// The local search's score: the same scale as a work row's.
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldList {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct WorkRow {
    /// Opaque, stable for the life of the session.
    pub row: String,
    pub work: WorkResult,
}

#[derive(Debug, Clone)]
pub struct SearchSnapshot {
    pub session: String,
    /// Moves whenever anything in the snapshot does.
    pub revision: u64,
    pub done: bool,
    pub query: QueryReading,
    /// Fixed from the first snapshot; empty for `Engine::search`, which has no such section.
    pub yours: Vec<HeldRow>,
    /// Empty until released.
    pub works: Vec<WorkRow>,
    pub providers: Vec<ProviderStatus>,
    pub warnings: Vec<Diagnostic>,
    /// Every provider that was asked could not be reached.
    pub offline: bool,
    pub correction: Option<Correction>,
}

/// Both strings are the user's query: never log above `trace!`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub searched: String,
    pub typed: String,
}

#[derive(Clone, Default)]
pub(crate) struct Extras {
    pub wellknown: Option<Arc<WellKnown>>,
    pub missing: Option<Unavailable>,
    pub literal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Progressive,
    ToDone,
}

pub(crate) struct Roster {
    pub providers: Arc<Vec<Box<dyn MetadataProvider>>>,
    pub availability: Vec<(ProviderId, ProviderAvailability)>,
}

/// Hash of the folded query and its shape, never the text, so the database holds no readable search
/// history.
pub(crate) fn query_key(q: &ParsedQuery, limit: u32) -> String {
    let f = |s: &Option<String>| s.as_deref().map(fold).unwrap_or_default();
    let canonical = [
        "v1".to_string(),
        limit.to_string(),
        q.isbn.clone().unwrap_or_default(),
        q.lang.clone().unwrap_or_default(),
        f(&q.fields.title),
        f(&q.fields.author),
        f(&q.fields.series),
        f(&q.fields.free),
        q.folded.clone(),
    ]
    .join("\u{1}");
    Sha256::digest(canonical.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Delivered rows keep their order; new exact matches enter above, others below by score.
pub fn stable_order(delivered: &[String], scored: &[(String, bool)]) -> Vec<String> {
    if delivered.is_empty() {
        return scored.iter().map(|(k, _)| k.clone()).collect();
    }
    let present: HashSet<&str> = scored.iter().map(|(k, _)| k.as_str()).collect();
    let was: HashSet<&str> = delivered.iter().map(String::as_str).collect();
    let mut out: Vec<String> = Vec::with_capacity(scored.len());
    out.extend(
        scored
            .iter()
            .filter(|(k, exact)| *exact && !was.contains(k.as_str()))
            .map(|(k, _)| k.clone()),
    );
    out.extend(
        delivered
            .iter()
            .filter(|k| present.contains(k.as_str()))
            .cloned(),
    );
    out.extend(
        scored
            .iter()
            .filter(|(k, exact)| !*exact && !was.contains(k.as_str()))
            .map(|(k, _)| k.clone()),
    );
    // `delivered` could repeat a key; the order must still be a permutation of `scored`.
    let mut seen = HashSet::new();
    out.retain(|k| seen.insert(k.clone()));
    out
}

fn is_exact(q: &ParsedQuery, typed: Option<&str>, w: &WorkResult) -> bool {
    if let Some(isbn) = typed {
        return w.has_isbn(isbn);
    }
    let Some(title) = w.title.as_deref().map(fold) else {
        return false;
    };
    let names: Vec<Vec<String>> = w
        .flat
        .authors
        .iter()
        .map(|a| {
            let mut t: Vec<String> = fold(a).split_whitespace().map(str::to_string).collect();
            t.sort();
            t
        })
        .collect();
    q.readings.iter().any(|r| {
        let (Some(rt), Some(ra)) = (&r.title, &r.author) else {
            return false;
        };
        let mut want: Vec<String> = ra.split_whitespace().map(str::to_string).collect();
        want.sort();
        title_key(rt) == title_key(&title) && names.contains(&want)
    })
}

struct Context {
    providers: Arc<Vec<Box<dyn MetadataProvider>>>,
    /// `None` means nothing kept; paused-clock tests use it because sqlx's pool times out under a
    /// paused clock.
    storage: Option<Storage>,
    query: ParsedQuery,
    plan: Plan,
    key: String,
    limit: u32,
    defaults: DefaultContext,
    typed: Option<String>,
    mode: Mode,
    scope: SearchScope,
    library: Option<Library>,
    yours: Vec<HeldRow>,
    wellknown: Option<Arc<WellKnown>>,
    known: ResultNames,
    fix: Option<Fix>,
    /// The library's books that answer the text as typed; a good one is the user's answer and is never
    /// corrected away.
    local: Vec<(Book, f64)>,
}

struct State {
    revision: u64,
    records: Vec<ProviderWork>,
    providers: Vec<ProviderStatus>,
    warnings: Vec<Diagnostic>,
    released: bool,
    delivered: Vec<String>,
    rows: HashMap<String, String>,
    works: Vec<WorkRow>,
    pending: usize,
    done: bool,
    asked: usize,
    not_reached: usize,
    likely: HashMap<String, Option<(i64, Option<String>)>>,
    notability: HashMap<String, u32>,
    correction: Option<Fix>,
    correcting: bool,
}

struct Shared {
    id: String,
    ctx: Context,
    state: Mutex<State>,
    tx: watch::Sender<u64>,
    /// Here rather than on [`SearchSession`] because a task starts the correction's.
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

pub struct SearchSession {
    shared: Arc<Shared>,
    touched: Mutex<tokio::time::Instant>,
}

impl Drop for SearchSession {
    fn drop(&mut self) {
        for t in lock(&self.shared.tasks).drain(..) {
            t.abort();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

type Asked = std::result::Result<Vec<ProviderWork>, Diagnostic>;

fn not_reached(d: &Diagnostic) -> bool {
    matches!(
        d.kind,
        DiagnosticKind::ProviderTimedOut { .. }
            | DiagnosticKind::ProviderFailed {
                class: ErrorClass::Network | ErrorClass::Timeout,
                ..
            }
    )
}

impl SearchSession {
    #[cfg(test)]
    pub(crate) async fn start(
        id: String,
        text: &str,
        scope: SearchScope,
        limit: u32,
        roster: Roster,
        storage: Option<Storage>,
        mode: Mode,
    ) -> Result<SearchSession> {
        Self::start_with(
            id,
            text,
            scope,
            limit,
            roster,
            storage,
            mode,
            Extras::default(),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    #[expect(clippy::too_many_lines)]
    pub(crate) async fn start_with(
        id: String,
        text: &str,
        scope: SearchScope,
        limit: u32,
        roster: Roster,
        storage: Option<Storage>,
        mode: Mode,
        extras: Extras,
    ) -> Result<SearchSession> {
        let first = parse(text, &KnowsNothing);
        let is_text = first.isbn.is_none() && !first.words.is_empty();
        let known = match (&extras.wellknown, is_text) {
            (Some(wk), true) => match wk.authors_named(&first.words).await {
                Ok(names) => ResultNames::named(names.iter().map(String::as_str)),
                Err(e) => {
                    tracing::debug!(class = ?ErrorClass::from(&e), "the offline index's names could not be read");
                    ResultNames::named([])
                }
            },
            _ => ResultNames::named([]),
        };
        let query = if is_text { parse(text, &known) } else { first };
        let plan = parsed::plan(&query, limit);
        let key = query_key(&query, limit);
        let typed = query.isbn.as_deref().and_then(|i| {
            if i.len() == 13 {
                Some(i.to_string())
            } else {
                crate::book::isbn10_to_13(i)
            }
        });
        let (library, library_language, local) = match &storage {
            Some(st) => {
                let library = Library::prepare(st.library_identities().await?);
                let local = if is_text || mode == Mode::Progressive {
                    st.search_books_ranked(
                        text,
                        &BookFilter::default(),
                        LibraryScope::All,
                        limit.max(1) as usize,
                    )
                    .await?
                } else {
                    Vec::new()
                };
                (Some(library), st.library_language().await?, local)
            }
            None => (None, None, Vec::new()),
        };
        let yours = match (&storage, mode) {
            (Some(st), Mode::Progressive) => yours_section(st, &local).await?,
            _ => Vec::new(),
        };
        let now = now_unix();
        let fix = if is_text && !extras.literal {
            candidate_fix(
                &query,
                extras.wellknown.as_deref(),
                storage.as_ref(),
                &local,
                now,
            )
            .await
        } else {
            None
        };
        let defaults = DefaultContext {
            typed_isbn: typed.clone(),
            held_isbns: library
                .as_ref()
                .map(Library::in_library_isbns)
                .unwrap_or_default(),
            language: query
                .lang
                .clone()
                .or(library_language)
                .unwrap_or_else(|| "en".into()),
        };
        let ctx = Context {
            providers: roster.providers.clone(),
            storage,
            query,
            plan,
            key,
            limit,
            defaults,
            typed,
            mode,
            scope,
            library,
            yours,
            wellknown: extras.wellknown.clone(),
            known,
            fix,
            local,
        };

        let mut records: Vec<ProviderWork> = Vec::new();
        let mut statuses: Vec<ProviderStatus> = Vec::new();
        let mut warnings: Vec<Diagnostic> = Vec::new();
        if is_text
            && let Some(why @ (Unavailable::NotInThisBuild | Unavailable::Failed(_))) =
                extras.missing
        {
            warnings.push(Diagnostic::offline_index_unavailable(why));
        }
        let mut to_ask: Vec<(usize, Option<i64>)> = Vec::new();
        for (pid, availability) in &roster.availability {
            let pid = *pid;
            let state = match availability {
                ProviderAvailability::Off => ProviderState::Off,
                ProviderAvailability::NoKey => ProviderState::NoKey,
                ProviderAvailability::Ready => {
                    let Some(idx) = roster.providers.iter().position(|p| p.id() == pid) else {
                        continue;
                    };
                    let kept = read_kept(&ctx, pid, now, &mut warnings).await?;
                    let kept_at = kept.as_ref().map(|(at, _)| *at);
                    if let Some((_, found)) = kept {
                        records.extend(found);
                    }
                    match (scope, kept_at) {
                        (SearchScope::LocalOnly, Some(at)) => {
                            ProviderState::FromCache { fetched_at: at }
                        }
                        (SearchScope::LocalOnly, None) => {
                            let found = from_index(&ctx, pid, now).await;
                            let at = found.as_ref().map(|(at, _)| *at);
                            if let Some((_, f)) = found {
                                records.extend(f);
                            }
                            ProviderState::NotAsked { kept: at }
                        }
                        (SearchScope::Everything, Some(at)) if now - at < FRESH_FOR => {
                            ProviderState::FromCache { fetched_at: at }
                        }
                        (SearchScope::Everything, kept) => {
                            to_ask.push((idx, kept));
                            ProviderState::Asking { kept }
                        }
                    }
                }
            };
            statuses.push(ProviderStatus {
                provider: pid,
                state,
            });
        }

        let notability = notability_of(ctx.wellknown.as_deref(), &records).await;
        let (tx, _) = watch::channel(0u64);
        let asked = to_ask.len();
        let shared = Arc::new(Shared {
            id,
            ctx,
            tasks: Mutex::new(Vec::new()),
            state: Mutex::new(State {
                revision: 0,
                records,
                providers: statuses,
                warnings,
                released: false,
                delivered: Vec::new(),
                rows: HashMap::new(),
                works: Vec::new(),
                pending: asked,
                done: asked == 0,
                asked,
                not_reached: 0,
                likely: HashMap::new(),
                notability,
                correction: None,
                correcting: false,
            }),
            tx,
        });
        {
            let mut st = lock(&shared.state);
            if st.done {
                settle(&shared, &mut st);
            }
            let ol_waiting = st.providers.iter().any(|s| {
                s.provider == ProviderId::OpenLibrary
                    && matches!(s.state, ProviderState::Asking { kept: None })
            });
            if st.done || (shared.ctx.mode == Mode::Progressive && !ol_waiting) {
                st.released = true;
            }
            recompute(&shared.ctx, &mut st);
            st.revision = 1;
        }
        let _ = shared.tx.send(1);

        let mut tasks = Vec::new();
        for (idx, kept) in to_ask {
            let s = shared.clone();
            tasks.push(tokio::spawn(
                async move { run_provider(s, idx, kept).await },
            ));
        }
        if lock(&shared.state).correcting {
            let s = shared.clone();
            tasks.push(tokio::spawn(async move { run_correction(s).await }));
        }
        if shared.ctx.mode == Mode::Progressive && !lock(&shared.state).released {
            let s = shared.clone();
            tasks.push(tokio::spawn(async move {
                tokio::time::sleep(RELEASE_AFTER).await;
                let mut st = lock(&s.state);
                if !st.released {
                    st.released = true;
                    recompute(&s.ctx, &mut st);
                    bump(&s, &mut st);
                }
            }));
        }
        tracing::debug!(asked, "search started");
        lock(&shared.tasks).extend(tasks);
        Ok(SearchSession {
            shared,
            touched: Mutex::new(tokio::time::Instant::now()),
        })
    }

    pub fn id(&self) -> &str {
        &self.shared.id
    }

    pub fn snapshot(&self) -> SearchSnapshot {
        let st = lock(&self.shared.state);
        snapshot_of(&self.shared, &st)
    }

    /// The next snapshot after `after_revision`, or the current one once `wait` (at most [`MAX_WAIT`])
    /// has passed; a finished session answers at once.
    pub async fn poll(&self, after_revision: u64, wait: Duration) -> SearchSnapshot {
        *lock(&self.touched) = tokio::time::Instant::now();
        let mut rx = self.shared.tx.subscribe();
        let ready = |rev: u64| rev > after_revision;
        {
            let st = lock(&self.shared.state);
            if ready(st.revision) || st.done {
                return snapshot_of(&self.shared, &st);
            }
        }
        let _ = tokio::time::timeout(wait.min(MAX_WAIT), async {
            while rx.changed().await.is_ok() {
                if ready(*rx.borrow()) {
                    break;
                }
            }
        })
        .await;
        self.snapshot()
    }

    pub(crate) async fn finish(&self) -> SearchSnapshot {
        let mut rx = self.shared.tx.subscribe();
        loop {
            if lock(&self.shared.state).done {
                return self.snapshot();
            }
            if rx.changed().await.is_err() {
                return self.snapshot();
            }
        }
    }

    pub fn cancel(&self) {
        for t in lock(&self.shared.tasks).drain(..) {
            t.abort();
        }
        let mut st = lock(&self.shared.state);
        if !st.done {
            st.done = true;
            st.pending = 0;
            st.released = true;
            recompute(&self.shared.ctx, &mut st);
            bump(&self.shared, &mut st);
        }
    }

    fn idle_for(&self, now: tokio::time::Instant) -> Duration {
        now.duration_since(*lock(&self.touched))
    }
}

async fn yours_section(storage: &Storage, found: &[(Book, f64)]) -> Result<Vec<HeldRow>> {
    let ids: Vec<i64> = found.iter().filter_map(|(b, _)| b.id).collect();
    let facts: HashMap<i64, crate::storage::HeldFacts> = storage
        .held_facts(&ids)
        .await?
        .into_iter()
        .map(|f| (f.book_id, f))
        .collect();
    Ok(found
        .iter()
        .filter_map(|(book, score)| {
            let f = facts.get(&book.id?)?;
            Some(HeldRow {
                in_library: f.in_library,
                has_copy: f.has_copy,
                passages: f.passages,
                notes: f.notes,
                lists: f
                    .lists
                    .iter()
                    .map(|(id, name)| HeldList {
                        id: *id,
                        name: name.clone(),
                    })
                    .collect(),
                score: *score,
                book: book.clone(),
            })
        })
        .collect())
}

/// A kept record that no longer reads makes the whole answer a miss, so the provider is asked
/// again.
async fn read_kept(
    ctx: &Context,
    provider: ProviderId,
    now: i64,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Option<(i64, Vec<ProviderWork>)>> {
    read_kept_as(ctx, provider, &ctx.key, &ctx.query, now, warnings).await
}

async fn read_kept_as(
    ctx: &Context,
    provider: ProviderId,
    key: &str,
    query: &ParsedQuery,
    now: i64,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Option<(i64, Vec<ProviderWork>)>> {
    let Some(storage) = ctx
        .storage
        .as_ref()
        .filter(|_| provider.defaults().kept_for.is_some())
    else {
        return Ok(None);
    };
    let Some(answer) = storage.kept_answer(key, provider, now).await? else {
        return Ok(None);
    };
    let mut out = Vec::with_capacity(answer.results.len());
    for (position, (entry, rows)) in answer.results.iter().enumerate() {
        match kept::decode(provider, rows, position) {
            Ok(mut w) => {
                if entry.by_author {
                    w.facts.found_by_author = Some(query.folded.clone());
                }
                out.push(w);
            }
            Err(e) => {
                tracing::debug!(provider = %provider, class = ?ErrorClass::from(&e),
                    "a kept record no longer reads; dropping it");
                for r in rows {
                    storage.drop_kept_record(provider, &r.key).await?;
                }
                warnings.push(Diagnostic::kept_record_unreadable(provider));
                return Ok(None);
            }
        }
    }
    Ok(Some((answer.fetched_at, out)))
}

async fn from_index(
    ctx: &Context,
    provider: ProviderId,
    now: i64,
) -> Option<(i64, Vec<ProviderWork>)> {
    let storage = ctx.storage.as_ref()?;
    if provider.defaults().kept_for.is_none() || ctx.query.isbn.is_some() {
        return None;
    }
    let rows: Vec<KeptRow> = storage
        .kept_records_matching(
            provider,
            &ctx.query.words,
            now,
            if ctx.limit == 0 {
                30
            } else {
                ctx.limit.min(40)
            },
        )
        .await
        .map_err(|e| tracing::debug!(class = ?ErrorClass::from(&e), "the offline index failed"))
        .ok()?;
    let oldest = rows.iter().map(|r| r.fetched_at).min()?;
    let found: Vec<ProviderWork> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| kept::decode(provider, std::slice::from_ref(r), i).ok())
        .collect();
    (!found.is_empty()).then_some((oldest, found))
}

/// A typed-search row explaining the text this well (`0..=1`, the text half of `rank`) stops a
/// correction when it is a work of standing.
pub(crate) const GOOD_TEXT: f64 = 0.9;
/// Editions at which a work has standing of its own: its title is real even one letter from a
/// famous one.
pub(crate) const STANDING_EDITIONS: u32 = 20;
/// Standing also holds for a work the offline index knows at least this fraction as well as the
/// fix.
const STANDING_SHARE: u32 = 4;
const CANDIDATES: usize = 40;

/// Kept records may fix a word but never vouch that a text is right: one can be a provider's
/// misspelt entry.
async fn candidate_fix(
    q: &ParsedQuery,
    wk: Option<&WellKnown>,
    storage: Option<&Storage>,
    local: &[(Book, f64)],
    now: i64,
) -> Option<Fix> {
    let mut targets: Vec<Target> = Vec::new();
    if let Some(wk) = wk {
        match wk.candidates(&q.words, CANDIDATES).await {
            Ok(hits) => {
                for h in hits {
                    for title in &h.titles {
                        targets.push(Target {
                            title: title.clone(),
                            author: h.author.clone(),
                            ol_works: h.ol_works.clone(),
                            notability: Some(h.sitelinks),
                        });
                    }
                }
            }
            Err(e) => {
                tracing::debug!(class = ?ErrorClass::from(&e), "the offline index could not be asked")
            }
        }
    }
    targets.extend(local.iter().filter_map(|(b, _)| target_of(b)));
    match correct::correct(q, &targets) {
        Verdict::Exact => return None,
        Verdict::Fix(f) => return Some(f),
        Verdict::Nothing => {}
    }
    let storage = storage?;
    let rows = storage
        .kept_records_matching(ProviderId::OpenLibrary, &q.words, now, 20)
        .await
        .ok()?;
    let kept: Vec<Target> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| kept::decode(ProviderId::OpenLibrary, std::slice::from_ref(r), i).ok())
        .filter_map(|w| target_of(&w.found.book))
        .collect();
    match correct::correct(q, &kept) {
        Verdict::Fix(f) => Some(f),
        _ => None,
    }
}

fn target_of(b: &Book) -> Option<Target> {
    Some(Target {
        title: b.title.clone()?,
        author: b.authors.first().cloned(),
        ol_works: b
            .openlibrary_key
            .iter()
            .filter(|k| k.starts_with("/works/"))
            .map(|k| k.trim_start_matches("/works/").to_string())
            .collect(),
        notability: None,
    })
}

async fn notability_of(wk: Option<&WellKnown>, records: &[ProviderWork]) -> HashMap<String, u32> {
    let Some(wk) = wk else {
        return HashMap::new();
    };
    let keys: Vec<(String, String, Option<String>)> = records
        .iter()
        .filter_map(|w| {
            let b = &w.found.book;
            let k = b.openlibrary_key.clone()?;
            let t = b.title.clone()?;
            k.starts_with("/works/")
                .then(|| (k, t, b.authors.first().cloned()))
        })
        .collect();
    if keys.is_empty() {
        return HashMap::new();
    }
    wk.notability(&keys).await.unwrap_or_else(|e| {
        tracing::debug!(class = ?ErrorClass::from(&e), "the offline index's notability could not be read");
        HashMap::new()
    })
}

fn reread(ctx: &Context, text: &str, ws: &[WorkResult]) -> ParsedQuery {
    let names = ResultNames::of(ws.iter().map(|w| &w.flat)).and(&ctx.known);
    parse(text, &names)
}

fn candidate_of<'a>(w: &'a WorkResult, notability: &HashMap<String, u32>) -> Candidate<'a> {
    Candidate {
        book: &w.flat,
        facts: &w.facts,
        sources: &w.sources,
        best_position: w.best_position,
        notability: w
            .flat
            .openlibrary_key
            .as_ref()
            .and_then(|k| notability.get(k).copied()),
    }
}

fn score_works(q: &ParsedQuery, notability: &HashMap<String, u32>, ws: &mut [WorkResult]) {
    for w in ws.iter_mut() {
        w.score = ranking::rank(q, &candidate_of(w, notability));
    }
}

fn is_fix_work(fix: &Fix, w: &WorkResult) -> bool {
    let mut titles: Vec<&str> = w.title.iter().map(String::as_str).collect();
    titles.extend(w.facts.also_titled.iter().map(String::as_str));
    correct::is_fixed_work(
        fix,
        w.flat.openlibrary_key.as_deref(),
        &titles,
        &w.flat.authors,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Keep,
    Rerank,
    Ask,
}

fn decide(ctx: &Context, st: &State, fix: &Fix) -> Decision {
    let mut ws = works(&st.records, &ctx.defaults);
    let typed = reread(ctx, &ctx.query.text, &ws);
    score_works(&typed, &st.notability, &mut ws);
    ws.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.best_position.cmp(&b.best_position))
    });
    if let Some(at) = ws.iter().position(|w| is_fix_work(fix, w)) {
        return if at == 0 {
            Decision::Keep
        } else {
            Decision::Rerank
        };
    }
    let floor = fix.notability.map_or(0, |n| n / STANDING_SHARE);
    let stands = ws.iter().any(|w| {
        let c = candidate_of(w, &st.notability);
        let known = c.notability.is_some_and(|n| n >= floor.max(1));
        let editions = w.facts.edition_count.unwrap_or(0) >= STANDING_EDITIONS;
        (known || editions) && ranking::text_part(&typed, &c) >= GOOD_TEXT
    });
    let yours = ctx.local.iter().any(|(b, score)| {
        *score >= ranking::TEXT * GOOD_TEXT
            && !correct::is_fixed_work(
                fix,
                b.openlibrary_key.as_deref(),
                &b.title.iter().map(String::as_str).collect::<Vec<_>>(),
                &b.authors,
            )
    });
    if stands || yours {
        Decision::Keep
    } else {
        Decision::Ask
    }
}

fn settle(s: &Shared, st: &mut State) {
    if st.correction.is_some() {
        return;
    }
    let Some(fix) = s.ctx.fix.clone() else {
        return;
    };
    match decide(&s.ctx, st, &fix) {
        Decision::Keep => {}
        Decision::Rerank => {
            st.correction = Some(fix);
            st.delivered.clear();
        }
        Decision::Ask => {
            st.correction = Some(fix);
            st.delivered.clear();
            st.correcting = true;
            st.done = false;
        }
    }
    tracing::debug!(decided = ?st.correction.is_some(), "a candidate correction was weighed");
}

async fn run_correction(s: Arc<Shared>) {
    let ctx = &s.ctx;
    let Some(fix) = lock(&s.state).correction.clone() else {
        return;
    };
    let text = match &fix.author {
        Some(a) => format!("{} by {a}", fix.title),
        None => fix.title.clone(),
    };
    let q = parse(&text, &KnowsNothing);
    let plan = parsed::plan(&q, ctx.limit);
    let key = query_key(&q, ctx.limit);
    let now = now_unix();
    let mut found: Vec<ProviderWork> = Vec::new();
    let mut warnings: Vec<Diagnostic> = Vec::new();
    for p in ctx.providers.iter() {
        let id = p.id();
        let kept = match read_kept_as(ctx, id, &key, &q, now, &mut warnings).await {
            Ok(k) => k,
            Err(e) => {
                tracing::debug!(class = ?ErrorClass::from(&e), "a kept correction could not be read");
                None
            }
        };
        let fresh = kept.as_ref().is_some_and(|(at, _)| now - at < FRESH_FOR);
        if ctx.scope == SearchScope::LocalOnly || fresh {
            found.extend(kept.into_iter().flat_map(|(_, w)| w));
            continue;
        }
        match ask(p.as_ref(), &q, &plan).await {
            Ok(w) => {
                keep_as(ctx, &key, id, &w, now).await;
                found.extend(w);
            }
            Err(d) => {
                parsed::log_degraded(id, &d);
                warnings.push(d);
                if let Some((at, w)) = kept {
                    warnings.push(Diagnostic::provider_answered_from_cache(id, at));
                    found.extend(w);
                }
            }
        }
    }
    let notability = notability_of(ctx.wellknown.as_deref(), &found).await;
    let mut st = lock(&s.state);
    st.notability.extend(notability);
    st.records.extend(found);
    for w in warnings {
        if !st.warnings.contains(&w) {
            st.warnings.push(w);
        }
    }
    st.correcting = false;
    st.done = true;
    st.released = true;
    st.delivered.clear();
    recompute(ctx, &mut st);
    bump(&s, &mut st);
}

async fn run_provider(s: Arc<Shared>, idx: usize, stale: Option<i64>) {
    let ctx = &s.ctx;
    let p = ctx.providers[idx].as_ref();
    let id = p.id();
    let asked = ask(p, &ctx.query, &ctx.plan).await;
    let now = now_unix();

    let (works, state, mut warnings, replace) = match asked {
        Ok(found) => {
            keep(ctx, id, &found, now).await;
            (found, ProviderState::Answered, Vec::new(), true)
        }
        Err(diagnostic) => {
            parsed::log_degraded(id, &diagnostic);
            let (kept, found) = match stale {
                Some(at) => (Some(at), Vec::new()),
                None => match from_index(ctx, id, now).await {
                    Some((at, found)) => (Some(at), found),
                    None => (None, Vec::new()),
                },
            };
            let mut warnings = vec![diagnostic.clone()];
            if let Some(at) = kept {
                warnings.push(Diagnostic::provider_answered_from_cache(id, at));
            }
            let state = match diagnostic.kind {
                DiagnosticKind::ProviderRateLimited { retry_after, .. } => {
                    ProviderState::RateLimited { retry_after, kept }
                }
                _ if not_reached(&diagnostic) => ProviderState::NotReached { diagnostic, kept },
                _ => ProviderState::Failed { diagnostic, kept },
            };
            (found, state, warnings, false)
        }
    };

    let notability = notability_of(ctx.wellknown.as_deref(), &works).await;
    let mut st = lock(&s.state);
    st.notability.extend(notability);
    if replace {
        st.records.retain(|w| w.found.provider != id);
    }
    if matches!(state, ProviderState::NotReached { .. }) {
        st.not_reached += 1;
    }
    st.records.extend(works);
    st.warnings.append(&mut warnings);
    if let Some(status) = st.providers.iter_mut().find(|p| p.provider == id) {
        status.state = state;
    }
    st.pending = st.pending.saturating_sub(1);
    if st.pending == 0 {
        st.done = true;
        settle(&s, &mut st);
    }
    if st.done || (ctx.mode == Mode::Progressive && id == ProviderId::OpenLibrary) {
        st.released = true;
    }
    recompute(ctx, &mut st);
    bump(&s, &mut st);
    let correcting = st.correcting;
    drop(st);
    if correcting {
        let task = s.clone();
        let handle = tokio::spawn(async move { run_correction(task).await });
        lock(&s.tasks).push(handle);
    }
}

/// An ISBN goes straight to the provider's edition lookup (typing an ISBN means that edition); text
/// goes through the plan.
async fn ask(p: &dyn MetadataProvider, q: &ParsedQuery, plan: &Plan) -> Asked {
    let id = p.id();
    let Some(isbn) = plan.primary.isbn.as_deref() else {
        return parsed::ask(p, q, plan).await;
    };
    match super::ask_provider(p, super::provider_deadline(), p.by_isbn(isbn)).await {
        Ok(found) => Ok(found
            .into_iter()
            .map(|pb| {
                let kept_record = id
                    .defaults()
                    .kept_for
                    .and_then(|_| kept::book_record(&pb.book));
                let mut w = ProviderWork::bare(pb);
                w.kept.extend(kept_record);
                w
            })
            .collect()),
        Err(d) => Err(d),
    }
}

/// A result with nothing to key it by would be a row missing from the repeat, so such an answer is
/// not kept at all.
async fn keep(ctx: &Context, provider: ProviderId, found: &[ProviderWork], now: i64) {
    keep_as(ctx, &ctx.key, provider, found, now).await;
}

async fn keep_as(ctx: &Context, key: &str, provider: ProviderId, found: &[ProviderWork], now: i64) {
    let Some(storage) = ctx.storage.as_ref() else {
        return;
    };
    if provider.defaults().kept_for.is_none() {
        return;
    }
    if found.iter().any(|w| w.kept.is_empty()) {
        tracing::debug!(provider = %provider, "an answer with a result that cannot be kept; not kept");
        return;
    }
    let entries: Vec<(KeptEntry, Vec<KeptRecord>)> = found
        .iter()
        .map(|w| {
            (
                KeptEntry {
                    records: w.kept.iter().map(|r| r.key.clone()).collect(),
                    by_author: w.facts.found_by_author.is_some(),
                },
                w.kept.clone(),
            )
        })
        .collect();
    if let Err(e) = storage.keep_answer(key, provider, &entries, now).await {
        tracing::debug!(provider = %provider, class = ?ErrorClass::from(&e), "keeping an answer failed");
    }
}

fn bump(s: &Shared, st: &mut State) {
    st.revision += 1;
    let _ = s.tx.send(st.revision);
}

fn recompute(ctx: &Context, st: &mut State) {
    let mut ws = works(&st.records, &ctx.defaults);
    let text = st
        .correction
        .as_ref()
        .map_or(ctx.query.text.as_str(), |f| f.searched.as_str());
    let reread = reread(ctx, text, &ws);
    score_works(&reread, &st.notability, &mut ws);
    ws.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.best_position.cmp(&b.best_position))
    });
    if let Some(library) = &ctx.library {
        for w in &mut ws {
            w.held = held_of(library, &mut st.likely, w);
        }
    }
    if !st.released {
        st.works.clear();
        return;
    }
    let scored: Vec<(String, bool)> = ws
        .iter()
        .map(|w| (w.key.clone(), is_exact(&reread, ctx.typed.as_deref(), w)))
        .collect();
    let order = stable_order(&st.delivered, &scored);
    let mut by_key: HashMap<String, WorkResult> =
        ws.into_iter().map(|w| (w.key.clone(), w)).collect();
    let shown = match ctx.mode {
        Mode::Progressive => ctx.limit.max(1) as usize,
        Mode::ToDone => usize::MAX,
    };
    let mut rows = Vec::new();
    for key in order.into_iter().take(shown) {
        let Some(work) = by_key.remove(&key) else {
            continue;
        };
        let next = st.rows.len() + 1;
        let row = st
            .rows
            .entry(key)
            .or_insert_with(|| format!("r{next}"))
            .clone();
        rows.push(WorkRow { row, work });
    }
    st.delivered = rows.iter().map(|r| r.work.key.clone()).collect();
    st.works = rows;
}

fn held_of(
    library: &Library,
    likely: &mut HashMap<String, Option<(i64, Option<String>)>>,
    w: &WorkResult,
) -> Held {
    if let Some(id) = library.certain(w) {
        return Held::Certain(id);
    }
    let mut words: Vec<&str> = w.title.iter().map(String::as_str).collect();
    words.extend(w.facts.also_titled.iter().map(String::as_str));
    words.push("\u{1}");
    words.extend(w.flat.authors.iter().map(String::as_str));
    let key = words.join("\u{2}");
    let found = likely
        .entry(key)
        .or_insert_with(|| library.likely(w))
        .clone();
    match found {
        Some((book_id, title)) => Held::Likely { book_id, title },
        None => Held::None,
    }
}

fn snapshot_of(s: &Shared, st: &State) -> SearchSnapshot {
    let q = &s.ctx.query;
    SearchSnapshot {
        session: s.id.clone(),
        revision: st.revision,
        done: st.done,
        query: QueryReading {
            readings: q.readings.iter().map(|r| r.kind).collect(),
            isbn: q.isbn.clone(),
            not_an_isbn: q.not_an_isbn,
        },
        yours: s.ctx.yours.clone(),
        works: st.works.clone(),
        providers: st.providers.clone(),
        warnings: st.warnings.clone(),
        offline: st.done && st.asked > 0 && st.not_reached == st.asked,
        correction: st.correction.as_ref().map(|f| Correction {
            searched: f.searched.clone(),
            typed: q.text.clone(),
        }),
    }
}

#[derive(Default)]
pub(crate) struct Registry {
    sessions: Mutex<Vec<Arc<SearchSession>>>,
    next: std::sync::atomic::AtomicU64,
}

impl Registry {
    /// Opaque; never reused within an engine.
    pub(crate) fn mint(&self) -> String {
        let n = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        format!("search-{n}")
    }

    fn prune(sessions: &mut Vec<Arc<SearchSession>>) {
        let now = tokio::time::Instant::now();
        sessions.retain(|s| s.idle_for(now) < SESSION_IDLE);
    }

    pub(crate) fn insert(&self, session: SearchSession) -> Arc<SearchSession> {
        let session = Arc::new(session);
        let mut all = lock(&self.sessions);
        Self::prune(&mut all);
        while all.len() >= MAX_SESSIONS {
            let now = tokio::time::Instant::now();
            let oldest = all
                .iter()
                .enumerate()
                .max_by_key(|(_, s)| s.idle_for(now))
                .map(|(i, _)| i)
                .expect("a non-empty list has an oldest");
            all.remove(oldest);
        }
        all.push(session.clone());
        session
    }

    pub(crate) fn get(&self, id: &str) -> Result<Arc<SearchSession>> {
        let mut all = lock(&self.sessions);
        Self::prune(&mut all);
        all.iter()
            .find(|s| s.id() == id)
            .cloned()
            .ok_or_else(|| EngineError::NotFound(format!("search session {id}")))
    }

    /// `false` when there was none: ending a search twice is not an error.
    pub(crate) fn end(&self, id: &str) -> bool {
        let gone = {
            let mut all = lock(&self.sessions);
            let at = all.iter().position(|s| s.id() == id);
            at.map(|i| all.remove(i))
        };
        match gone {
            Some(s) => {
                s.cancel();
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_already_shown_stay_and_new_ones_go_below_unless_exact() {
        let d = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let s = |v: &[(&str, bool)]| {
            v.iter()
                .map(|(k, e)| (k.to_string(), *e))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            stable_order(&[], &s(&[("c", false), ("a", true), ("b", false)])),
            d(&["c", "a", "b"])
        );
        assert_eq!(
            stable_order(
                &d(&["a", "b"]),
                &s(&[
                    ("x", false),
                    ("b", false),
                    ("e", true),
                    ("a", false),
                    ("y", false)
                ])
            ),
            d(&["e", "a", "b", "x", "y"])
        );
        assert_eq!(
            stable_order(&d(&["a", "gone", "b"]), &s(&[("b", false), ("a", false)])),
            d(&["a", "b"])
        );
    }

    #[test]
    fn the_query_key_is_a_hash_of_the_folded_query() {
        let k = |t: &str| query_key(&parse(t, &KnowsNothing), 20);
        assert_eq!(k("Dune"), k("dune"));
        assert_eq!(k("Café"), k("cafe"));
        assert_ne!(k("dune"), k("dune messiah"));
        assert_ne!(k("title:dune"), k("dune"), "the shape is part of it");
        assert_ne!(
            query_key(&parse("dune", &KnowsNothing), 20),
            query_key(&parse("dune", &KnowsNothing), 40)
        );
        let key = k("the left hand of darkness");
        assert_eq!(key.len(), 64);
        assert!(!key.contains("darkness"));
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn no_row_shown_moves_below_a_later_one_but_an_exact_match(
            n in 0usize..12,
            delivered_mask in any::<u16>(),
            exact_mask in any::<u16>(),
            gone_mask in any::<u16>(),
            seed in any::<u64>(),
        ) {
            let keys: Vec<String> = (0..n).map(|i| format!("w{i}")).collect();
            let mut delivered: Vec<String> = keys
                .iter()
                .enumerate()
                .filter(|(i, _)| delivered_mask & (1 << i) != 0)
                .map(|(_, k)| k.clone())
                .collect();
            let mut s = seed;
            for i in (1..delivered.len()).rev() {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                delivered.swap(i, (s >> 33) as usize % (i + 1));
            }
            let scored: Vec<(String, bool)> = keys
                .iter()
                .enumerate()
                .filter(|(i, _)| gone_mask & (1 << i) == 0 || delivered_mask & (1 << i) == 0)
                .map(|(i, k)| (k.clone(), exact_mask & (1 << i) != 0))
                .collect();
            let out = stable_order(&delivered, &scored);

            let mut a = out.clone();
            a.sort();
            let mut b: Vec<String> = scored.iter().map(|(k, _)| k.clone()).collect();
            b.sort();
            prop_assert_eq!(a, b, "not a permutation of the current rows");

            let pos = |k: &str| out.iter().position(|x| x == k);
            let kept: Vec<&String> = delivered.iter().filter(|k| pos(k).is_some()).collect();
            for w in kept.windows(2) {
                prop_assert!(pos(w[0]) < pos(w[1]), "shown rows reordered: {:?}", out);
            }
            if !delivered.is_empty() {
                for (k, exact) in &scored {
                    if delivered.contains(k) || *exact {
                        continue;
                    }
                    for d in &kept {
                        prop_assert!(pos(d) < pos(k), "{} shown, moved below new {}: {:?}", d, k, out);
                    }
                }
            }
            let new: Vec<usize> = scored
                .iter()
                .filter(|(k, e)| !*e && !delivered.contains(k))
                .filter_map(|(k, _)| pos(k))
                .collect();
            prop_assert!(new.windows(2).all(|w| w[0] < w[1]));
        }
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use crate::providers::{KeptRecord, ProviderBook, RecordForm, RecordKind, SearchRequest};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    enum Answer {
        Works(Vec<(&'static str, &'static str)>),
        Unreachable,
        Hangs,
    }

    struct Fake {
        id: ProviderId,
        answer: Mutex<Answer>,
        delay: Duration,
        asked: Arc<AtomicUsize>,
    }

    impl Fake {
        fn quick(id: ProviderId, answer: Answer) -> (Box<dyn MetadataProvider>, Arc<AtomicUsize>) {
            Self::slow(id, answer, Duration::ZERO)
        }
        fn slow(
            id: ProviderId,
            answer: Answer,
            delay: Duration,
        ) -> (Box<dyn MetadataProvider>, Arc<AtomicUsize>) {
            let asked = Arc::new(AtomicUsize::new(0));
            (
                Box::new(Fake {
                    id,
                    answer: Mutex::new(answer),
                    delay,
                    asked: asked.clone(),
                }),
                asked,
            )
        }
    }

    fn ol_work(title: &str, author: &str, n: usize, position: usize) -> ProviderWork {
        let key = format!("/works/OL{n}W");
        let body = serde_json::json!({
            "key": key, "title": title, "author_name": [author], "edition_count": 10 - n.min(9)
        })
        .to_string();
        let mut w = crate::providers::openlibrary::decode_doc(&body, None, position).unwrap();
        w.kept = vec![KeptRecord {
            key,
            kind: RecordKind::Work,
            form: RecordForm::Doc,
            title: Some(title.into()),
            authors: vec![author.into()],
            body,
        }];
        w
    }

    fn gb_volume(title: &str, author: &str, n: usize, position: usize) -> ProviderWork {
        ProviderWork::bare(ProviderBook {
            book: crate::book::Book {
                title: Some(title.into()),
                authors: vec![author.into()],
                googlebooks_id: Some(format!("gb{n}")),
                description: Some("only Google says this".into()),
                ..Default::default()
            },
            provider: ProviderId::GoogleBooks,
            position,
        })
    }

    #[async_trait]
    impl MetadataProvider for Fake {
        fn id(&self) -> ProviderId {
            self.id
        }
        async fn search(&self, req: &SearchRequest) -> Result<Vec<ProviderBook>> {
            Ok(self
                .search_works(req)
                .await?
                .into_iter()
                .map(|w| w.found)
                .collect())
        }
        async fn search_works(&self, _req: &SearchRequest) -> Result<Vec<ProviderWork>> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            let answer = lock(&self.answer).clone();
            match answer {
                Answer::Works(list) => Ok(list
                    .iter()
                    .enumerate()
                    .map(|(i, (t, a))| match self.id {
                        ProviderId::OpenLibrary => ol_work(t, a, i + 1, i),
                        ProviderId::GoogleBooks => gb_volume(t, a, i + 1, i),
                    })
                    .collect()),
                Answer::Unreachable => Err(EngineError::Provider {
                    provider: self.id,
                    message: "error sending request: connection refused".into(),
                }),
                Answer::Hangs => {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            }
        }
        async fn by_isbn(&self, _: &str) -> Result<Option<ProviderBook>> {
            Ok(None)
        }
    }

    fn roster(list: Vec<Box<dyn MetadataProvider>>) -> Roster {
        let ids: Vec<ProviderId> = list.iter().map(|p| p.id()).collect();
        Roster {
            providers: Arc::new(list),
            availability: ProviderId::ALL
                .into_iter()
                .map(|id| {
                    let a = if ids.contains(&id) {
                        ProviderAvailability::Ready
                    } else {
                        ProviderAvailability::NoKey
                    };
                    (id, a)
                })
                .collect(),
        }
    }

    async fn storage() -> Storage {
        Storage::connect("sqlite::memory:").await.unwrap()
    }

    async fn run(
        storage: &Storage,
        list: Vec<Box<dyn MetadataProvider>>,
        text: &str,
    ) -> SearchSnapshot {
        SearchSession::start(
            "t".into(),
            text,
            SearchScope::Everything,
            20,
            roster(list),
            Some(storage.clone()),
            Mode::ToDone,
        )
        .await
        .unwrap()
        .finish()
        .await
    }

    fn titles(s: &SearchSnapshot) -> Vec<String> {
        s.works
            .iter()
            .map(|r| r.work.title.clone().unwrap_or_default())
            .collect()
    }

    fn state_of(s: &SearchSnapshot, p: ProviderId) -> ProviderState {
        s.providers
            .iter()
            .find(|x| x.provider == p)
            .map(|x| x.state.clone())
            .unwrap()
    }

    fn dunes() -> Answer {
        Answer::Works(vec![
            ("Dune", "Frank Herbert"),
            ("Dune Messiah", "Frank Herbert"),
            ("Children of Dune", "Frank Herbert"),
        ])
    }

    #[tokio::test]
    async fn a_repeat_search_asks_nothing_and_keeps_its_order() {
        let s = storage().await;
        let (ol, asked) = Fake::quick(ProviderId::OpenLibrary, dunes());
        let first = run(&s, vec![ol], "dune").await;
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(
            state_of(&first, ProviderId::OpenLibrary),
            ProviderState::Answered
        );
        assert_eq!(
            state_of(&first, ProviderId::GoogleBooks),
            ProviderState::NoKey
        );

        let (ol, asked) = Fake::quick(ProviderId::OpenLibrary, Answer::Hangs);
        let again = run(&s, vec![ol], "Dune").await;
        assert_eq!(
            asked.load(Ordering::SeqCst),
            0,
            "a fresh kept answer was asked again"
        );
        assert_eq!(titles(&again), titles(&first));
        assert!(matches!(
            state_of(&again, ProviderId::OpenLibrary),
            ProviderState::FromCache { .. }
        ));
        assert!(again.warnings.is_empty());
    }

    #[tokio::test]
    async fn offline_the_kept_answer_stands_in_and_says_so() {
        let s = storage().await;
        let (ol, _) = Fake::quick(ProviderId::OpenLibrary, dunes());
        let first = run(&s, vec![ol], "dune").await;
        s.age_kept_searches(2 * FRESH_FOR).await.unwrap();

        let (ol, asked) = Fake::quick(ProviderId::OpenLibrary, Answer::Unreachable);
        let offline = run(&s, vec![ol], "dune").await;
        assert_eq!(
            asked.load(Ordering::SeqCst),
            1,
            "a stale answer is refreshed"
        );
        assert_eq!(titles(&offline), titles(&first));
        assert!(offline.offline);
        assert!(matches!(
            state_of(&offline, ProviderId::OpenLibrary),
            ProviderState::NotReached { kept: Some(_), .. }
        ));
        let kinds: Vec<_> = offline.warnings.iter().map(|d| d.kind.clone()).collect();
        assert!(matches!(
            kinds[0],
            DiagnosticKind::ProviderFailed {
                class: ErrorClass::Network,
                ..
            }
        ));
        assert!(matches!(
            kinds[1],
            DiagnosticKind::ProviderAnsweredFromCache { .. }
        ));

        let (ol, _) = Fake::quick(ProviderId::OpenLibrary, Answer::Unreachable);
        let other = run(&s, vec![ol], "messiah herbert").await;
        assert_eq!(
            titles(&other).first().map(String::as_str),
            Some("Dune Messiah")
        );
        assert!(titles(&other).contains(&"Dune Messiah".to_string()));
        assert!(other.offline);

        let (ol, _) = Fake::quick(ProviderId::OpenLibrary, Answer::Unreachable);
        let nothing = run(&s, vec![ol], "middlemarch").await;
        assert!(nothing.works.is_empty());
        assert!(matches!(
            state_of(&nothing, ProviderId::OpenLibrary),
            ProviderState::NotReached { kept: None, .. }
        ));
        assert_eq!(nothing.warnings.len(), 1);
    }

    #[tokio::test]
    async fn google_books_is_shown_and_never_kept() {
        let s = storage().await;
        let (ol, _) = Fake::quick(ProviderId::OpenLibrary, dunes());
        let (gb, _) = Fake::quick(
            ProviderId::GoogleBooks,
            Answer::Works(vec![
                ("Dune", "Frank Herbert"),
                ("Dune: House Atreides", "Brian Herbert"),
            ]),
        );
        let snap = run(&s, vec![ol, gb], "dune").await;
        let dune = snap
            .works
            .iter()
            .find(|r| r.work.title.as_deref() == Some("Dune"))
            .unwrap();
        assert_eq!(
            dune.work.sources,
            vec![ProviderId::OpenLibrary, ProviderId::GoogleBooks]
        );
        assert_eq!(
            dune.work.flattened().description.as_deref(),
            Some("only Google says this")
        );
        assert!(titles(&snap).contains(&"Dune: House Atreides".to_string()));
        assert_eq!(
            s.kept_providers().await.unwrap(),
            vec![
                ("provider_records".to_string(), "openlibrary".to_string()),
                ("search_cache".to_string(), "openlibrary".to_string()),
            ]
        );
        let leaked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM provider_records
              WHERE body LIKE '%only Google%' OR body LIKE '%gb1%' OR title LIKE '%Atreides%'",
        )
        .fetch_one(s.pool())
        .await
        .unwrap();
        assert_eq!(leaked, 0);
    }

    #[tokio::test]
    async fn a_local_only_search_asks_no_provider() {
        let s = storage().await;
        let (ol, _) = Fake::quick(ProviderId::OpenLibrary, dunes());
        run(&s, vec![ol], "dune").await;
        let mine = s
            .upsert_book(&shelf_book("Dune Messiah", "Frank Herbert"), None)
            .await
            .unwrap();
        let (ol, asked) = Fake::quick(ProviderId::OpenLibrary, dunes());
        let session = SearchSession::start(
            "t".into(),
            "dune messiah",
            SearchScope::LocalOnly,
            20,
            roster(vec![ol]),
            Some(s.clone()),
            Mode::Progressive,
        )
        .await
        .unwrap();
        let snap = session.snapshot();
        assert!(snap.done);
        assert_eq!(asked.load(Ordering::SeqCst), 0);
        assert!(titles(&snap).contains(&"Dune Messiah".to_string()));
        assert_eq!(snap.yours.len(), 1);
        assert_eq!(
            snap.yours[0].book.id,
            Some(mine),
            "your own book, asked of nobody"
        );
        assert!(matches!(
            state_of(&snap, ProviderId::OpenLibrary),
            ProviderState::NotAsked { kept: Some(_) }
        ));
    }

    fn shelf_book(title: &str, author: &str) -> crate::book::Book {
        crate::book::Book {
            title: Some(title.into()),
            authors: vec![author.into()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn yours_comes_first_and_every_work_says_what_is_held() {
        let s = storage().await;
        let dune = s
            .upsert_book(
                &crate::book::Book {
                    openlibrary_key: Some("/works/OL1W".into()),
                    ..shelf_book("Dune", "Frank Herbert")
                },
                None,
            )
            .await
            .unwrap();
        let messiah = s
            .upsert_book(&shelf_book("Dune Messiah", "Frank Herbert"), None)
            .await
            .unwrap();
        s.upsert_book(&shelf_book("Emma", "Jane Austen"), None)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO copies (book_id, tenure, learned_from, created_at, updated_at)
             VALUES (?, 'own', 'user', 0, 0)",
        )
        .bind(dune)
        .execute(s.pool())
        .await
        .unwrap();

        let (ol, _) = Fake::slow(ProviderId::OpenLibrary, dunes(), Duration::from_millis(100));
        let session = SearchSession::start(
            "t".into(),
            "dune",
            SearchScope::Everything,
            20,
            roster(vec![ol]),
            Some(s.clone()),
            Mode::Progressive,
        )
        .await
        .unwrap();
        let first = session.snapshot();
        assert!(first.works.is_empty(), "Open Library has not answered");
        let yours: Vec<(Option<i64>, bool)> = first
            .yours
            .iter()
            .map(|r| (r.book.id, r.in_library))
            .collect();
        assert_eq!(yours, vec![(Some(dune), true), (Some(messiah), false)]);

        let done = session.finish().await;
        assert_eq!(done.yours.len(), 2, "nothing a provider says moves it");
        let held = |t: &str| {
            done.works
                .iter()
                .find(|r| r.work.title.as_deref() == Some(t))
                .map(|r| r.work.held.clone())
                .unwrap()
        };
        assert_eq!(held("Dune"), Held::Certain(dune));
        assert_eq!(
            held("Dune Messiah"),
            Held::Likely {
                book_id: messiah,
                title: Some("Dune Messiah".into())
            }
        );
        assert_eq!(
            held("Children of Dune"),
            Held::None,
            "the band is not shown"
        );
    }

    #[tokio::test]
    async fn an_unreadable_kept_record_is_dropped_and_asked_again() {
        let s = storage().await;
        let (ol, _) = Fake::quick(ProviderId::OpenLibrary, dunes());
        run(&s, vec![ol], "dune").await;
        sqlx::query("UPDATE provider_records SET body = '[]' WHERE key = '/works/OL2W'")
            .execute(s.pool())
            .await
            .unwrap();
        let (ol, asked) = Fake::quick(ProviderId::OpenLibrary, dunes());
        let snap = run(&s, vec![ol], "dune").await;
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(snap.works.len(), 3);
        assert!(matches!(
            snap.warnings[0].kind,
            DiagnosticKind::KeptRecordUnreadable {
                provider: ProviderId::OpenLibrary
            }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn rows_are_released_once_and_late_ones_go_below() {
        let (gb, _) = Fake::quick(
            ProviderId::GoogleBooks,
            Answer::Works(vec![("Emma", "Jane Austen"), ("Dune", "Frank Herbert")]),
        );
        let (ol, _) = Fake::slow(ProviderId::OpenLibrary, dunes(), Duration::from_secs(3));
        let session = SearchSession::start(
            "t".into(),
            "dune",
            SearchScope::Everything,
            20,
            roster(vec![ol, gb]),
            None,
            Mode::Progressive,
        )
        .await
        .unwrap();
        let first = session.snapshot();
        assert!(first.works.is_empty(), "shown before release");

        let mut snap = session.poll(first.revision, MAX_WAIT).await;
        while snap.works.is_empty() {
            snap = session.poll(snap.revision, MAX_WAIT).await;
        }
        let released_at = tokio::time::Instant::now();
        let shown = titles(&snap);
        assert_eq!(shown.len(), 2);
        assert!(!snap.done);

        while !snap.done {
            snap = session.poll(snap.revision, MAX_WAIT).await;
        }
        assert!(released_at.elapsed() < Duration::from_secs(3));
        let all = titles(&snap);
        assert_eq!(&all[..2], &shown[..]);
        assert_eq!(all.len(), 4, "{all:?}");
        let dune = snap
            .works
            .iter()
            .find(|r| r.work.title.as_deref() == Some("Dune"))
            .unwrap();
        assert_eq!(dune.work.sources.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_registry_keeps_eight_and_forgets_the_idle() {
        let reg = Registry::default();
        let mut ids = Vec::new();
        for _ in 0..9 {
            let (ol, _) = Fake::quick(ProviderId::OpenLibrary, Answer::Hangs);
            let session = SearchSession::start(
                reg.mint(),
                "dune",
                SearchScope::Everything,
                20,
                roster(vec![ol]),
                None,
                Mode::Progressive,
            )
            .await
            .unwrap();
            ids.push(session.id().to_string());
            reg.insert(session);
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        assert!(
            matches!(reg.get(&ids[0]), Err(EngineError::NotFound(_))),
            "the ninth let the first go"
        );
        assert!(reg.get(&ids[8]).is_ok());
        assert!(reg.end(&ids[8]));
        assert!(!reg.end(&ids[8]), "ending twice is ending");
        assert!(reg.get("search-nope").is_err());
        tokio::time::advance(SESSION_IDLE).await;
        assert!(reg.get(&ids[1]).is_err(), "idle ten minutes");
    }

    #[tokio::test]
    async fn ending_a_session_stops_its_providers() {
        struct Guard(Arc<AtomicUsize>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        struct Forever(Arc<AtomicUsize>);
        #[async_trait]
        impl MetadataProvider for Forever {
            fn id(&self) -> ProviderId {
                ProviderId::OpenLibrary
            }
            async fn search(&self, _: &SearchRequest) -> Result<Vec<ProviderBook>> {
                let _g = Guard(self.0.clone());
                std::future::pending::<()>().await;
                unreachable!()
            }
            async fn by_isbn(&self, _: &str) -> Result<Option<ProviderBook>> {
                Ok(None)
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let s = storage().await;
        let reg = Registry::default();
        let session = SearchSession::start(
            reg.mint(),
            "dune",
            SearchScope::Everything,
            20,
            roster(vec![Box::new(Forever(dropped.clone()))]),
            Some(s),
            Mode::Progressive,
        )
        .await
        .unwrap();
        let id = session.id().to_string();
        reg.insert(session);
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert!(reg.end(&id));
        tokio::task::yield_now().await;
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            1,
            "the provider's future outlived its session"
        );
    }
}

#[cfg(test)]
mod correction_tests {
    use super::*;
    use crate::providers::{KeptRecord, ProviderBook, RecordForm, RecordKind, SearchRequest};
    use crate::search::wellknown::{Unavailable, fixture};
    use async_trait::async_trait;

    type Doc = (&'static str, &'static str, u32, u32);

    struct ByRequest {
        script: fn(&SearchRequest) -> Vec<Doc>,
        asked: Arc<Mutex<Vec<SearchRequest>>>,
    }

    fn doc(d: &Doc, position: usize) -> ProviderWork {
        let (title, author, n, editions) = *d;
        let key = format!("/works/OL{n}W");
        let body = serde_json::json!({
            "key": key, "title": title, "author_name": [author], "edition_count": editions
        })
        .to_string();
        let mut w = crate::providers::openlibrary::decode_doc(&body, None, position).unwrap();
        w.kept = vec![KeptRecord {
            key,
            kind: RecordKind::Work,
            form: RecordForm::Doc,
            title: Some(title.into()),
            authors: vec![author.into()],
            body,
        }];
        w
    }

    #[async_trait]
    impl MetadataProvider for ByRequest {
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
            lock(&self.asked).push(req.clone());
            Ok((self.script)(req)
                .iter()
                .enumerate()
                .map(|(i, d)| doc(d, i))
                .collect())
        }
        async fn by_isbn(&self, _: &str) -> Result<Option<ProviderBook>> {
            Ok(None)
        }
    }

    fn open_library(req: &SearchRequest) -> Vec<Doc> {
        let title = req.title.as_deref().unwrap_or_default();
        let q = req.query.as_deref().unwrap_or_default();
        match (title, q) {
            ("Dune", _) => vec![
                ("Dune", "Frank Herbert", 893414, 240),
                ("Dune Messiah", "Frank Herbert", 893461, 120),
            ],
            ("Jane Eyre", _) => vec![("Jane Eyre", "Charlotte Brontë", 1095427, 900)],
            (_, "jane eyer") => vec![("Jane Eyer", "Charlotte Brontë", 77, 2)],
            (_, "the hobit") => vec![
                ("The Hobbit Companion", "David Day", 5, 6),
                ("The Hobbit", "J. R. R. Tolkien", 27482, 600),
            ],
            (_, "neuromancr") => vec![("Neuromancr", "A. Small Press", 99, 40)],
            (_, "dune") => vec![
                ("Dune", "Another Dune", 1234, 240),
                ("Dune", "Frank Herbert", 893414, 240),
            ],
            _ => Vec::new(),
        }
    }

    async fn search(
        storage: &Storage,
        text: &str,
        extras: Extras,
    ) -> (SearchSnapshot, Vec<SearchRequest>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let p: Box<dyn MetadataProvider> = Box::new(ByRequest {
            script: open_library,
            asked: asked.clone(),
        });
        let roster = Roster {
            providers: Arc::new(vec![p]),
            availability: vec![(ProviderId::OpenLibrary, ProviderAvailability::Ready)],
        };
        let done = SearchSession::start_with(
            "t".into(),
            text,
            SearchScope::Everything,
            20,
            roster,
            Some(storage.clone()),
            Mode::ToDone,
            extras,
        )
        .await
        .unwrap()
        .finish()
        .await;
        let asked = lock(&asked).clone();
        (done, asked)
    }

    async fn with_index() -> Extras {
        Extras {
            wellknown: Some(Arc::new(fixture::index().await)),
            missing: None,
            literal: false,
        }
    }

    fn first(s: &SearchSnapshot) -> String {
        s.works
            .first()
            .and_then(|r| r.work.title.clone())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn a_misspelling_that_finds_nothing_is_searched_as_corrected() {
        let st = Storage::connect("sqlite::memory:").await.unwrap();
        let (snap, asked) = search(&st, "dnue herbrt", with_index().await).await;
        assert_eq!(
            snap.correction,
            Some(Correction {
                searched: "dune herbert".into(),
                typed: "dnue herbrt".into(),
            })
        );
        assert_eq!(first(&snap), "Dune");
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert_eq!(asked[1].title.as_deref(), Some("Dune"));
        assert_eq!(asked[1].author.as_deref(), Some("Frank Herbert"));
        assert!(snap.done);

        let (again, asked) = search(&st, "dnue herbrt", with_index().await).await;
        assert!(asked.is_empty(), "{asked:?}");
        assert_eq!(again.correction, snap.correction);
        assert_eq!(first(&again), "Dune");
    }

    #[tokio::test]
    async fn a_search_as_typed_is_not_corrected() {
        let st = Storage::connect("sqlite::memory:").await.unwrap();
        let mut extras = with_index().await;
        extras.literal = true;
        let (snap, asked) = search(&st, "dnue herbrt", extras).await;
        assert_eq!(snap.correction, None);
        assert!(snap.works.is_empty());
        assert_eq!(asked.len(), 1);
    }

    #[tokio::test]
    async fn a_stray_misspelt_record_does_not_stop_the_fix() {
        let st = Storage::connect("sqlite::memory:").await.unwrap();
        let (snap, _) = search(&st, "jane eyer", with_index().await).await;
        assert_eq!(
            snap.correction.as_ref().map(|c| c.searched.as_str()),
            Some("jane eyre")
        );
        assert_eq!(first(&snap), "Jane Eyre");
    }

    #[tokio::test]
    async fn a_good_answer_of_standing_is_not_corrected_away() {
        let st = Storage::connect("sqlite::memory:").await.unwrap();
        let (snap, asked) = search(&st, "neuromancr", with_index().await).await;
        assert_eq!(snap.correction, None);
        assert_eq!(first(&snap), "Neuromancr");
        assert_eq!(asked.len(), 1);
    }

    #[tokio::test]
    async fn a_fix_already_found_is_ranked_not_asked() {
        let st = Storage::connect("sqlite::memory:").await.unwrap();
        let (snap, asked) = search(&st, "the hobit", with_index().await).await;
        assert_eq!(first(&snap), "The Hobbit");
        assert_eq!(asked.len(), 1, "{asked:?}");
    }

    #[tokio::test]
    async fn a_title_in_the_index_is_never_corrected() {
        let st = Storage::connect("sqlite::memory:").await.unwrap();
        let (snap, asked) = search(&st, "the habit", with_index().await).await;
        assert_eq!(snap.correction, None);
        assert_eq!(asked.len(), 1);
    }

    #[tokio::test]
    async fn a_missing_index_is_said_and_the_search_still_runs() {
        let st = Storage::connect("sqlite::memory:").await.unwrap();
        let extras = Extras {
            wellknown: None,
            missing: Some(Unavailable::NotInThisBuild),
            literal: false,
        };
        let (snap, asked) = search(&st, "dnue herbrt", extras).await;
        assert_eq!(snap.correction, None);
        assert_eq!(asked.len(), 1);
        let said: Vec<String> = snap.warnings.iter().map(|d| d.to_string()).collect();
        assert_eq!(
            said,
            vec!["offline index: not in this build; searched without it"]
        );

        let extras = Extras {
            wellknown: None,
            missing: Some(Unavailable::NotConfigured),
            literal: false,
        };
        let (snap, _) = search(&st, "dnue herbrt", extras).await;
        assert!(snap.warnings.is_empty());
    }

    #[tokio::test]
    async fn notability_breaks_a_tie_between_equal_matches() {
        let st = Storage::connect("sqlite::memory:").await.unwrap();
        let (snap, _) = search(&st, "dune", with_index().await).await;
        assert_eq!(snap.correction, None);
        let top = &snap.works[0].work;
        assert_eq!(top.authors, vec!["Frank Herbert".to_string()]);
        let none = Extras::default();
        let (snap, _) = search(&st, "dune", none).await;
        assert_eq!(snap.works[0].work.authors, vec!["Another Dune".to_string()]);
    }
}
