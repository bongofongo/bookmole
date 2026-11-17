//! History statistics over time and the read summaries the Library table shares, pure over what
//! storage read. There is deliberately no current-run streak; days are UTC, weeks start Monday.

use std::collections::{BTreeMap, HashMap, HashSet};

use time::{Date, Duration, Month, OffsetDateTime};

use crate::Engine;
use crate::book::{Book, ReadingState};
use crate::error::{EngineError, Result};
use crate::progress::Progress;
use crate::spreadsheet::inert_cell;
use crate::storage::{
    BookQuery, DayFact, MarkStamp, Rating, ReadCount, ReadFacts, ReadWindow, Reading, now_unix,
    run_cutoff, runs_of_days,
};

/// The UTC day an instant falls on, or `None` outside `time`'s range (the window sentinels).
pub fn date_of(ts: i64) -> Option<Date> {
    OffsetDateTime::from_unix_timestamp(ts)
        .ok()
        .map(|d| d.date())
}

pub fn midnight(d: Date) -> i64 {
    d.midnight().assume_utc().unix_timestamp()
}

pub fn fmt_day(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

pub fn parse_day(s: &str) -> Option<Date> {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    Date::parse(s, fmt).ok()
}

fn fmt_month(d: Date) -> String {
    format!("{:04}-{:02}", d.year(), u8::from(d.month()))
}

/// The same date a year earlier; 29 February becomes the 28th.
fn year_before(d: Date) -> Date {
    let y = d.year() - 1;
    Date::from_calendar_date(y, d.month(), d.day())
        .or_else(|_| Date::from_calendar_date(y, d.month(), d.day() - 1))
        .unwrap_or(d)
}

fn last_of_month(d: Date) -> Date {
    let first = d.replace_day(1).unwrap_or(d);
    let next = if d.month() == Month::December {
        Date::from_calendar_date(d.year() + 1, Month::January, 1)
    } else {
        Date::from_calendar_date(d.year(), d.month().next(), 1)
    };
    next.map(|n| n - Duration::days(1)).unwrap_or(first)
}

pub fn days_between(from: i64, to: i64) -> Option<i64> {
    Some((date_of(to)? - date_of(from)?).whole_days())
}

/// Unix seconds, `since` inclusive, `until` exclusive; either end may be open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistorySpan {
    pub since: Option<i64>,
    pub until: Option<i64>,
}

impl HistorySpan {
    /// Refuses a span that ends before it starts (`InvalidInput`), which would yield confident zeros.
    pub fn new(since: Option<i64>, until: Option<i64>) -> Result<HistorySpan> {
        if let (Some(s), Some(u)) = (since, until)
            && s >= u
        {
            return Err(EngineError::InvalidInput(format!(
                "the span {s}..{u} ends before it starts"
            )));
        }
        Ok(HistorySpan { since, until })
    }

    pub fn contains(&self, t: i64) -> bool {
        self.since.is_none_or(|s| t >= s) && self.until.is_none_or(|u| t < u)
    }

    pub fn overlaps(&self, a: i64, b: i64) -> bool {
        self.since.is_none_or(|s| b >= s) && self.until.is_none_or(|u| a < u)
    }

    /// The span's days, inclusive, clipped to `today`. An open start begins at
    /// `first_known` (the earliest day the library has any fact on). `None` when
    /// no day is left.
    pub fn days(&self, first_known: Option<Date>, today: Date) -> Option<(Date, Date)> {
        let first = match self.since {
            Some(s) => date_of(s)?,
            None => first_known?,
        };
        let last = match self.until {
            Some(u) => date_of(u - 1)?.min(today),
            None => today,
        };
        (first <= last).then_some((first, last))
    }

    fn contains_day(&self, d: Date) -> bool {
        let start = midnight(d);
        self.overlaps(start, start + 86_399)
    }
}

#[derive(Debug, Clone)]
pub struct HistoryRead {
    pub book: Book,
    pub reading: Reading,
    pub progress: Progress,
    pub count: ReadCount,
    pub state: ReadingState,
    /// `readings.put_down_at`, else `finished_at` for a reading a merge closed `abandoned`; `None` for
    /// older put-down reads, never a guess.
    pub put_down_at: Option<i64>,
    pub ended_at: Option<i64>,
    /// Where the card sits and what a span is tested against: `ended_at`, else now if open, else
    /// `last_modified` (an upper bound, used only for placement).
    pub at: i64,
    pub days: Option<i64>,
    pub rating: Option<Rating>,
    pub passages: i64,
    pub notes: i64,
    /// Every `YYYY-MM` (UTC) month from the read's start to `at`; a read with no start spans its end
    /// month.
    pub months: Vec<String>,
}

impl HistoryRead {
    /// When the read began, for span tests: `started_at`, else its end, else when we learned of it;
    /// never after `at`.
    pub fn begin(&self) -> i64 {
        self.reading
            .started_at
            .or(self.ended_at)
            .unwrap_or(self.reading.created_at)
            .min(self.at)
    }

    /// Whether the timeline can place this read: it has a start and a finish, or it is open and being
    /// read. A put-down read never is, and no date is guessed to make one placeable.
    pub fn is_placeable(&self) -> bool {
        match self.state {
            ReadingState::Abandoned => false,
            ReadingState::Reading if self.reading.finished_at.is_none() => true,
            _ => self.reading.started_at.is_some() && self.reading.finished_at.is_some(),
        }
    }

    pub fn in_progress_at(&self, t: i64) -> bool {
        if self.begin() > t {
            return false;
        }
        match self.state {
            // A put-down read with no instant cannot be placed, so it is never claimed as under way.
            ReadingState::Abandoned => self.put_down_at.is_some_and(|p| p > t),
            _ => self.ended_at.is_none_or(|e| e > t),
        }
    }
}

/// A reflection or review names its reading and wins. An ordinary note goes to the reading of its
/// book whose window holds `created_at`; ties go to the later start, then the higher id.
pub fn attribute_note(note: &MarkStamp, windows: &[ReadWindow]) -> Option<i64> {
    if note.reading_id.is_some() {
        return note.reading_id;
    }
    let book = note.book_id?;
    windows
        .iter()
        .filter(|w| w.book_id == book && w.start <= note.at && note.at <= w.end)
        .max_by_key(|w| (w.start, w.reading_id))
        .map(|w| w.reading_id)
}

#[derive(Debug, Clone, Default)]
pub struct HistoryFacts {
    pub reads: Vec<ReadFacts>,
    pub books: HashMap<i64, Book>,
    pub events: Vec<DayFact>,
    pub passages: Vec<MarkStamp>,
    pub notes: Vec<MarkStamp>,
    pub windows: Vec<ReadWindow>,
}

impl HistoryFacts {
    pub fn first_known(&self) -> Option<Date> {
        let events = self.events.iter().filter_map(|e| parse_day(&e.day));
        let marks = self
            .passages
            .iter()
            .chain(&self.notes)
            .filter_map(|m| date_of(m.at));
        let reads = self.reads.iter().filter_map(|r| {
            r.reading
                .started_at
                .or(r.reading.finished_at)
                .and_then(date_of)
        });
        events.chain(marks).chain(reads).min()
    }

    fn note_reads(&self) -> HashMap<i64, Option<i64>> {
        self.notes
            .iter()
            .map(|n| (n.id, attribute_note(n, &self.windows)))
            .collect()
    }
}

/// Readings whose book is missing (impossible under the foreign key) are skipped.
pub fn summarise_reads(f: &HistoryFacts, now: i64) -> Vec<HistoryRead> {
    let mut passages: HashMap<i64, i64> = HashMap::new();
    for p in &f.passages {
        if let Some(r) = p.reading_id {
            *passages.entry(r).or_default() += 1;
        }
    }
    let mut notes: HashMap<i64, i64> = HashMap::new();
    for r in f.note_reads().into_values().flatten() {
        *notes.entry(r).or_default() += 1;
    }
    f.reads
        .iter()
        .filter_map(|rf| {
            let book = f.books.get(&rf.reading.book_id)?.clone();
            Some(summarise(rf, book, &passages, &notes, now))
        })
        .collect()
}

fn summarise(
    rf: &ReadFacts,
    book: Book,
    passages: &HashMap<i64, i64>,
    notes: &HashMap<i64, i64>,
    now: i64,
) -> HistoryRead {
    let r = &rf.reading;
    let state = ReadingState::from(r.status.as_str());
    let put_down_at = r.put_down_at;
    let ended_at = match state {
        ReadingState::Abandoned => put_down_at,
        _ => r.finished_at,
    };
    let at = match (ended_at, &state) {
        (Some(e), _) => e,
        (None, ReadingState::Abandoned) => r.last_modified,
        (None, _) => now,
    };
    let days = match (r.started_at, ended_at) {
        (Some(s), Some(e)) => days_between(s, e),
        _ => None,
    };
    let progress = Progress::of_reading(r, book.page_count);
    let mut read = HistoryRead {
        book,
        reading: r.clone(),
        progress,
        count: rf.count,
        state,
        put_down_at,
        ended_at,
        at,
        days,
        rating: rf.rating.clone(),
        passages: passages.get(&r.id).copied().unwrap_or(0),
        notes: notes.get(&r.id).copied().unwrap_or(0),
        months: Vec::new(),
    };
    read.months = months_between(read.begin(), read.at);
    read
}

fn months_between(a: i64, b: i64) -> Vec<String> {
    let (Some(a), Some(b)) = (date_of(a), date_of(b)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut m = a.replace_day(1).unwrap_or(a);
    while m <= b {
        out.push(fmt_month(m));
        m = last_of_month(m) + Duration::days(1);
    }
    out
}

/// Placeable reads whose `[begin, at]` meets the span, newest end first (`at`, then reading id,
/// descending). The others stay in the book's reads and in every count.
pub fn timeline(reads: &[HistoryRead], span: &HistorySpan) -> Vec<HistoryRead> {
    let mut out: Vec<HistoryRead> = reads
        .iter()
        .filter(|r| r.is_placeable() && span.overlaps(r.begin(), r.at))
        .cloned()
        .collect();
    out.sort_by(|a, b| b.at.cmp(&a.at).then(b.reading.id.cmp(&a.reading.id)));
    out
}

/// Open reads (`ReadingState::Reading` only: a put-down read still has `finished_at IS NULL`),
/// most recently begun first, then reading id descending. No span: now is not a period.
pub fn reading_now(reads: &[HistoryRead]) -> Vec<HistoryRead> {
    let mut out: Vec<HistoryRead> = reads
        .iter()
        .filter(|r| r.state == ReadingState::Reading && r.ended_at.is_none())
        .cloned()
        .collect();
    out.sort_by(|a, b| {
        b.begin()
            .cmp(&a.begin())
            .then(b.reading.id.cmp(&a.reading.id))
    });
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceName {
    pub key: String,
    pub label: String,
}

/// The proper product name of a claimant; a frontend wanting other words keys on `key`.
pub fn source_label(source: &str) -> String {
    match source {
        "koreader" => "KOReader".into(),
        "local" => "bookmole".into(),
        "goodreads" => "Goodreads".into(),
        "calibre" => "calibre".into(),
        "manual" => "By hand".into(),
        "vault" => "Notes".into(),
        "user" => "Typed in".into(),
        "migrated" => "Earlier records".into(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayRun {
    pub days: i64,
    pub first: String,
    pub last: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryFigures {
    pub books_finished: i64,
    pub reading_now: i64,
    pub put_down: i64,
    /// Pages the log measured in the span; `None` when nothing measured any.
    pub pages: Option<i64>,
    /// Days the span has had so far — the denominator of every per-day rate.
    pub elapsed_days: i64,
    pub pages_per_day: Option<f64>,
    pub minutes: Option<i64>,
    pub minutes_from: Vec<SourceName>,
    pub passages: i64,
    pub passages_per_finished_book: Option<f64>,
    pub notes: i64,
    pub reviews: i64,
    pub reflections: i64,
    /// The longest ended run of consecutive active days, clipped to the span; ties go to the most
    /// recent.
    pub longest_run: Option<DayRun>,
}

pub fn figures(
    f: &HistoryFacts,
    reads: &[HistoryRead],
    span: &HistorySpan,
    now: i64,
) -> HistoryFigures {
    let today = date_of(now).unwrap_or(Date::MIN);
    let days = span.days(f.first_known(), today);
    let elapsed_days = days.map_or(0, |(a, b)| (b - a).whole_days() + 1);

    let finished: Vec<&HistoryRead> = reads
        .iter()
        .filter(|r| {
            r.state == ReadingState::Finished && r.ended_at.is_some_and(|e| span.contains(e))
        })
        .collect();
    let end_t = span.until.map_or(now, |u| (u - 1).min(now));
    let reading_now = reads.iter().filter(|r| r.in_progress_at(end_t)).count() as i64;
    let put_down = reads
        .iter()
        .filter(|r| r.state == ReadingState::Abandoned && span.contains(r.at))
        .count() as i64;

    let in_span_events: Vec<&DayFact> = f
        .events
        .iter()
        .filter(|e| match (parse_day(&e.day), days) {
            (Some(d), Some((a, b))) => d >= a && d <= b,
            _ => false,
        })
        .collect();
    let pages = sum_known(in_span_events.iter().map(|e| e.pages));
    let minutes = sum_known(in_span_events.iter().map(|e| e.minutes));
    let mut by_source: BTreeMap<&str, i64> = BTreeMap::new();
    for e in &in_span_events {
        if let Some(m) = e.minutes {
            *by_source.entry(e.source.as_str()).or_default() += m;
        }
    }
    let mut sources: Vec<(&str, i64)> = by_source.into_iter().collect();
    sources.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let minutes_from = sources
        .into_iter()
        .map(|(k, _)| SourceName {
            key: k.to_string(),
            label: source_label(k),
        })
        .collect();

    let passages = f.passages.iter().filter(|p| span.contains(p.at)).count() as i64;
    let notes: Vec<&MarkStamp> = f.notes.iter().filter(|n| span.contains(n.at)).collect();
    let kind = |k: &str| notes.iter().filter(|n| n.kind == k).count() as i64;

    let passages_per_finished_book = (!finished.is_empty())
        .then(|| finished.iter().map(|r| r.passages).sum::<i64>() as f64 / finished.len() as f64);

    HistoryFigures {
        books_finished: finished.len() as i64,
        reading_now,
        put_down,
        pages,
        elapsed_days,
        pages_per_day: pages
            .filter(|_| elapsed_days > 0)
            .map(|p| p as f64 / elapsed_days as f64),
        minutes,
        minutes_from,
        passages,
        passages_per_finished_book,
        notes: notes.len() as i64,
        reviews: kind("review"),
        reflections: kind("reflection"),
        longest_run: longest_ended_run(&f.events, span, now),
    }
}

/// `None` when every value is `None`: a period nothing measured has no pages, not zero.
fn sum_known(values: impl Iterator<Item = Option<i64>>) -> Option<i64> {
    values.fold(None, |acc, v| match (acc, v) {
        (a, None) => a,
        (None, Some(v)) => Some(v),
        (Some(a), Some(v)) => Some(a + v),
    })
}

/// Runs are computed over the whole log and filtered by the ended cutoff before clipping, so a run
/// still going today is excluded whole rather than cut at a span edge.
pub fn longest_ended_run(events: &[DayFact], span: &HistorySpan, now: i64) -> Option<DayRun> {
    let cutoff = run_cutoff(now).ok()?;
    let mut days: Vec<String> = events.iter().map(|e| e.day.clone()).collect();
    days.sort();
    days.dedup();
    let mut best: Option<(i64, Date, Date)> = None;
    for (a, b, _) in runs_of_days(&days) {
        let (Some(a), Some(b)) = (parse_day(&a), parse_day(&b)) else {
            continue;
        };
        if b > cutoff {
            continue;
        }
        let mut lo = a;
        while lo <= b && !span.contains_day(lo) {
            lo += Duration::days(1);
        }
        let mut hi = b;
        while hi >= lo && !span.contains_day(hi) {
            hi -= Duration::days(1);
        }
        if lo > hi {
            continue;
        }
        let len = (hi - lo).whole_days() + 1;
        if best.is_none_or(|(l, _, h)| len > l || (len == l && hi > h)) {
            best = Some((len, lo, hi));
        }
    }
    best.map(|(days, first, last)| DayRun {
        days,
        first: fmt_day(first),
        last: fmt_day(last),
    })
}

#[derive(Debug, Clone)]
pub struct HistoryRecord {
    pub read: HistoryRead,
    pub pages: Option<i64>,
    pub pages_per_day: Option<f64>,
    pub marks: i64,
    pub tied_with: Vec<HistoryRead>,
}

#[derive(Debug, Clone, Default)]
pub struct HistoryRecords {
    pub fastest: Option<HistoryRecord>,
    pub longest: Option<HistoryRecord>,
    pub most_marked: Option<HistoryRecord>,
    /// Highest rating, compared as a fraction of its own scale so different scales can meet.
    pub best_rated: Option<HistoryRecord>,
}

fn rating_fraction(r: &Rating) -> Option<f64> {
    let span = r.scale.max - r.scale.min;
    (span > 0.0).then(|| (r.value - r.scale.min) / span)
}

/// Every tie goes to the most recent finish, then the higher reading id, so no record depends on
/// load order. Ratings on different scales meet as a fraction of their own scale.
pub fn records(reads: &[HistoryRead], span: &HistorySpan) -> HistoryRecords {
    let finished: Vec<&HistoryRead> = reads
        .iter()
        .filter(|r| {
            r.state == ReadingState::Finished && r.ended_at.is_some_and(|e| span.contains(e))
        })
        .collect();

    fn pick<K: PartialOrd + Copy>(
        reads: &[&HistoryRead],
        key: impl Fn(&HistoryRead) -> Option<K>,
    ) -> Option<HistoryRecord> {
        let recency = |r: &HistoryRead| (r.at, r.reading.id);
        let mut best: Option<(&HistoryRead, K)> = None;
        for r in reads {
            let Some(k) = key(r) else { continue };
            best = match best {
                None => Some((r, k)),
                Some((b, bk)) => {
                    if k > bk || (k == bk && recency(r) > recency(b)) {
                        Some((r, k))
                    } else {
                        Some((b, bk))
                    }
                }
            };
        }
        let (winner, k) = best?;
        let mut tied_with: Vec<HistoryRead> = reads
            .iter()
            .filter(|r| r.reading.id != winner.reading.id && key(r) == Some(k))
            .map(|r| (*r).clone())
            .collect();
        tied_with.sort_by_key(|r| std::cmp::Reverse((r.at, r.reading.id)));
        let pages = winner.book.page_count.filter(|p| *p > 0);
        Some(HistoryRecord {
            pages,
            pages_per_day: match (pages, winner.days) {
                (Some(p), Some(d)) => Some(p as f64 / d.max(1) as f64),
                _ => None,
            },
            marks: winner.passages + winner.notes,
            tied_with,
            read: winner.clone(),
        })
    }

    HistoryRecords {
        fastest: pick(&finished, |r| r.days.map(|d| -d)),
        longest: pick(&finished, |r| r.book.page_count.filter(|p| *p > 0)),
        most_marked: pick(&finished, |r| Some(r.passages + r.notes).filter(|m| *m > 0)),
        best_rated: pick(&finished, |r| r.rating.as_ref().and_then(rating_fraction)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryMetric {
    Pages,
    Minutes,
    BooksFinished,
    Passages,
    Notes,
}

impl HistoryMetric {
    /// Pages and minutes are measured: an unmeasured bucket has no value, not zero.
    pub fn is_measured(self) -> bool {
        matches!(self, HistoryMetric::Pages | HistoryMetric::Minutes)
    }

    fn slug(self) -> &'static str {
        match self {
            HistoryMetric::Pages => "pages",
            HistoryMetric::Minutes => "minutes",
            HistoryMetric::BooksFinished => "books-finished",
            HistoryMetric::Passages => "passages",
            HistoryMetric::Notes => "notes",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryGroup {
    Day,
    Week,
    Month,
}

impl HistoryGroup {
    pub fn period_of(self, d: Date) -> Date {
        match self {
            HistoryGroup::Day => d,
            HistoryGroup::Week => {
                d - Duration::days(i64::from(d.weekday().number_days_from_monday()))
            }
            HistoryGroup::Month => d.replace_day(1).unwrap_or(d),
        }
    }

    fn period_end(self, start: Date) -> Date {
        match self {
            HistoryGroup::Day => start,
            HistoryGroup::Week => start + Duration::days(6),
            HistoryGroup::Month => last_of_month(start),
        }
    }

    /// The comparable day a year earlier: same weekday 52 weeks back for a week, same date otherwise.
    fn last_year(self, d: Date) -> Date {
        match self {
            HistoryGroup::Week => d - Duration::days(364),
            _ => year_before(d),
        }
    }

    fn slug(self) -> &'static str {
        match self {
            HistoryGroup::Day => "day",
            HistoryGroup::Week => "week",
            HistoryGroup::Month => "month",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySplit {
    Unsplit,
    Book,
    /// One part per claimant (`reading_events.source`, `readings.source`, `highlights.source`,
    /// `vault`). No device identity is recorded, so KOReader is one part however many readers it ran
    /// on.
    Device,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryCompare {
    Nothing,
    LastYear,
    Average,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeriesQuery {
    pub metric: HistoryMetric,
    pub group: HistoryGroup,
    pub split: HistorySplit,
    pub compare: HistoryCompare,
    pub span: HistorySpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayPoint {
    pub day: Date,
    pub book_id: Option<i64>,
    pub source: String,
    pub value: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesPart {
    pub key: String,
    pub label: String,
    pub value: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesBucket {
    pub period: String,
    /// The first and last day counted: the bucket clipped to the span and today. Buckets partition the
    /// span's days.
    pub start: String,
    pub end: String,
    pub value: i64,
    /// For pages and minutes: whether anything measured this bucket. `false` is no data, not zero;
    /// always `true` for counted metrics.
    pub known: bool,
    /// The split, largest first (then key); empty when unsplit. Sums to `value`.
    pub parts: Vec<SeriesPart>,
    pub compare: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistorySeries {
    pub buckets: Vec<SeriesBucket>,
    pub total: i64,
    pub known: bool,
    pub average: Option<f64>,
    /// Index of the largest bucket (earliest of equals); `None` when every bucket is zero.
    pub busiest: Option<usize>,
    /// The comparison buckets summed: the same span a year earlier to date, since buckets stop at
    /// today.
    pub compare_total: Option<i64>,
    pub compare_change: Option<f64>,
}

pub fn points(f: &HistoryFacts, reads: &[HistoryRead], metric: HistoryMetric) -> Vec<DayPoint> {
    match metric {
        HistoryMetric::Pages | HistoryMetric::Minutes => f
            .events
            .iter()
            .filter_map(|e| {
                let v = if metric == HistoryMetric::Pages {
                    e.pages
                } else {
                    e.minutes
                }?;
                Some(DayPoint {
                    day: parse_day(&e.day)?,
                    book_id: Some(e.book_id),
                    source: e.source.clone(),
                    value: v,
                })
            })
            .collect(),
        HistoryMetric::BooksFinished => reads
            .iter()
            .filter(|r| r.state == ReadingState::Finished)
            .filter_map(|r| {
                Some(DayPoint {
                    day: date_of(r.ended_at?)?,
                    book_id: Some(r.reading.book_id),
                    source: r.reading.source.clone(),
                    value: 1,
                })
            })
            .collect(),
        HistoryMetric::Passages | HistoryMetric::Notes => {
            let marks = if metric == HistoryMetric::Passages {
                &f.passages
            } else {
                &f.notes
            };
            marks
                .iter()
                .filter_map(|m| {
                    Some(DayPoint {
                        day: date_of(m.at)?,
                        book_id: m.book_id,
                        source: m.source.clone(),
                        value: 1,
                    })
                })
                .collect()
        }
    }
}

pub fn buckets_of(group: HistoryGroup, first: Date, last: Date) -> Vec<(Date, Date, Date)> {
    let mut out = Vec::new();
    let mut period = group.period_of(first);
    while period <= last {
        let end = group.period_end(period);
        out.push((period, period.max(first), end.min(last)));
        period = end + Duration::days(1);
    }
    out
}

fn part_key(split: HistorySplit, p: &DayPoint) -> String {
    match split {
        HistorySplit::Unsplit => String::new(),
        HistorySplit::Book => p.book_id.map_or_else(|| "none".into(), |b| b.to_string()),
        HistorySplit::Device => p.source.clone(),
    }
}

pub fn series(
    pts: &[DayPoint],
    q: &SeriesQuery,
    days: Option<(Date, Date)>,
    labels: &HashMap<i64, String>,
) -> HistorySeries {
    let Some((first, last)) = days else {
        return HistorySeries {
            buckets: Vec::new(),
            total: 0,
            known: !q.metric.is_measured(),
            average: None,
            busiest: None,
            compare_total: None,
            compare_change: None,
        };
    };
    let mut daily: BTreeMap<Date, i64> = BTreeMap::new();
    let mut daily_known: HashSet<Date> = HashSet::new();
    for p in pts {
        *daily.entry(p.day).or_default() += p.value;
        daily_known.insert(p.day);
    }
    let range_sum = |a: Date, b: Date| -> i64 { daily.range(a..=b).map(|(_, v)| v).sum() };
    let range_known = |a: Date, b: Date| -> bool { daily.range(a..=b).next().is_some() };

    let shape = buckets_of(q.group, first, last);
    let mut parts_by_bucket: Vec<BTreeMap<String, i64>> = vec![BTreeMap::new(); shape.len()];
    if q.split != HistorySplit::Unsplit {
        for p in pts {
            if p.day < first || p.day > last {
                continue;
            }
            let period = q.group.period_of(p.day);
            if let Ok(i) = shape.binary_search_by(|(per, _, _)| per.cmp(&period)) {
                *parts_by_bucket[i].entry(part_key(q.split, p)).or_default() += p.value;
            }
        }
    }

    let mut buckets: Vec<SeriesBucket> = shape
        .iter()
        .zip(parts_by_bucket)
        .map(|(&(period, start, end), parts)| {
            let mut parts: Vec<SeriesPart> = parts
                .into_iter()
                .map(|(key, value)| SeriesPart {
                    label: match q.split {
                        HistorySplit::Book => key
                            .parse::<i64>()
                            .ok()
                            .and_then(|id| labels.get(&id).cloned())
                            .unwrap_or_default(),
                        _ => source_label(&key),
                    },
                    key,
                    value,
                })
                .collect();
            parts.sort_by(|a, b| b.value.cmp(&a.value).then(a.key.cmp(&b.key)));
            SeriesBucket {
                period: fmt_day(period),
                start: fmt_day(start),
                end: fmt_day(end),
                value: range_sum(start, end),
                known: !q.metric.is_measured() || range_known(start, end),
                parts,
                compare: None,
            }
        })
        .collect();

    let total: i64 = buckets.iter().map(|b| b.value).sum();
    let average = (!buckets.is_empty()).then(|| total as f64 / buckets.len() as f64);
    let busiest = buckets
        .iter()
        .enumerate()
        .filter(|(_, b)| b.value > 0)
        .fold(None::<(usize, i64)>, |best, (i, b)| match best {
            Some((_, v)) if v >= b.value => best,
            _ => Some((i, b.value)),
        })
        .map(|(i, _)| i);

    let mut compare_total = None;
    match q.compare {
        HistoryCompare::Nothing => {}
        HistoryCompare::Average => {
            for b in &mut buckets {
                b.compare = average;
            }
        }
        HistoryCompare::LastYear => {
            let mut sum = 0;
            for (b, &(_, start, end)) in buckets.iter_mut().zip(&shape) {
                let v = range_sum(q.group.last_year(start), q.group.last_year(end));
                sum += v;
                b.compare = Some(v as f64);
            }
            compare_total = Some(sum);
        }
    }
    let compare_change = compare_total
        .filter(|c| *c > 0)
        .map(|c| (total - c) as f64 / c as f64);

    HistorySeries {
        known: !q.metric.is_measured() || buckets.iter().any(|b| b.known),
        buckets,
        total,
        average,
        busiest,
        compare_total,
        compare_change,
    }
}

#[derive(Debug, Clone)]
pub struct KeptRow {
    pub book: Book,
    pub passages: i64,
    pub notes: i64,
}

/// Passages and notes dated in the span, per book, most first (passages + notes, then passages,
/// then title order, then id). A negative `limit` is no limit; notes with no book are not counted.
pub fn kept(
    f: &HistoryFacts,
    span: &HistorySpan,
    title_order: &HashMap<i64, i64>,
    limit: i64,
) -> Vec<KeptRow> {
    let mut tally: HashMap<i64, (i64, i64)> = HashMap::new();
    for p in f.passages.iter().filter(|p| span.contains(p.at)) {
        if let Some(b) = p.book_id {
            tally.entry(b).or_default().0 += 1;
        }
    }
    for n in f.notes.iter().filter(|n| span.contains(n.at)) {
        if let Some(b) = n.book_id {
            tally.entry(b).or_default().1 += 1;
        }
    }
    let mut rows: Vec<KeptRow> = tally
        .into_iter()
        .filter_map(|(id, (passages, notes))| {
            Some(KeptRow {
                book: f.books.get(&id)?.clone(),
                passages,
                notes,
            })
        })
        .collect();
    let order = |r: &KeptRow| {
        let id = r.book.id.unwrap_or_default();
        (title_order.get(&id).copied().unwrap_or(i64::MAX), id)
    };
    rows.sort_by(|a, b| {
        (b.passages + b.notes)
            .cmp(&(a.passages + a.notes))
            .then(b.passages.cmp(&a.passages))
            .then(order(a).cmp(&order(b)))
    });
    if limit >= 0 {
        rows.truncate(limit as usize);
    }
    rows
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedCsv {
    pub path: std::path::PathBuf,
    pub rows: i64,
}

/// `start,end,period,part,label,value,compare`: a total row per bucket (empty `part`), then one row
/// per part when split. Free-text cells go through [`inert_cell`] so none is a live formula.
pub fn series_csv(s: &HistorySeries) -> Result<(String, i64)> {
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record([
        "start", "end", "period", "part", "label", "value", "compare",
    ])
    .map_err(csv_err)?;
    let mut rows = 0;
    for b in &s.buckets {
        let compare = b.compare.map(|c| format!("{c}")).unwrap_or_default();
        let value = if b.known {
            b.value.to_string()
        } else {
            String::new()
        };
        w.write_record([&b.start, &b.end, &b.period, "", "", &value, &compare])
            .map_err(csv_err)?;
        rows += 1;
        for p in &b.parts {
            w.write_record([
                b.start.as_str(),
                b.end.as_str(),
                b.period.as_str(),
                &inert_cell(&p.key),
                &inert_cell(&p.label),
                &p.value.to_string(),
                "",
            ])
            .map_err(csv_err)?;
            rows += 1;
        }
    }
    let bytes = w
        .into_inner()
        .map_err(|e| EngineError::Other(e.to_string()))?;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), rows))
}

fn csv_err(e: csv::Error) -> EngineError {
    EngineError::Other(format!("writing the history CSV: {e}"))
}

impl Engine {
    pub(crate) async fn history_facts(&self, with_events: bool) -> Result<HistoryFacts> {
        let books = self
            .storage
            .list_books(&BookQuery::default())
            .await?
            .into_iter()
            .filter_map(|b| Some((b.id?, b)))
            .collect();
        Ok(HistoryFacts {
            reads: self.storage.history_reads().await?,
            books,
            events: if with_events {
                self.storage.history_events(None, None).await?
            } else {
                Vec::new()
            },
            passages: self.storage.passage_stamps().await?,
            notes: self.storage.note_stamps_for_history().await?,
            windows: self.storage.read_windows().await?,
        })
    }

    pub async fn history_timeline(&self, span: &HistorySpan) -> Result<Vec<HistoryRead>> {
        let f = self.history_facts(false).await?;
        Ok(timeline(&summarise_reads(&f, now_unix()), span))
    }

    pub async fn history_reading_now(&self) -> Result<Vec<HistoryRead>> {
        let f = self.history_facts(false).await?;
        Ok(reading_now(&summarise_reads(&f, now_unix())))
    }

    pub async fn history_figures(&self, span: &HistorySpan) -> Result<HistoryFigures> {
        let now = now_unix();
        let f = self.history_facts(true).await?;
        let reads = summarise_reads(&f, now);
        Ok(figures(&f, &reads, span, now))
    }

    pub async fn history_records(&self, span: &HistorySpan) -> Result<HistoryRecords> {
        let f = self.history_facts(false).await?;
        Ok(records(&summarise_reads(&f, now_unix()), span))
    }

    pub async fn history_series(&self, q: &SeriesQuery) -> Result<HistorySeries> {
        let now = now_unix();
        let f = self.history_facts(true).await?;
        let reads = summarise_reads(&f, now);
        let today = date_of(now).unwrap_or(Date::MIN);
        let days = q.span.days(f.first_known(), today);
        let labels: HashMap<i64, String> = f
            .books
            .iter()
            .map(|(id, b)| (*id, b.title.clone().unwrap_or_default()))
            .collect();
        Ok(series(&points(&f, &reads, q.metric), q, days, &labels))
    }

    pub async fn history_kept(&self, span: &HistorySpan, limit: i64) -> Result<Vec<KeptRow>> {
        let f = self.history_facts(false).await?;
        let title_order = self.title_order().await?;
        Ok(kept(&f, span, &title_order, limit))
    }

    /// Writes `history-<metric>-<group>-<UTC stamp>.csv` to [`Engine::exports_dir`]; a second export in
    /// the same second gets a suffix rather than overwriting.
    pub async fn export_history_series(&self, q: &SeriesQuery) -> Result<SavedCsv> {
        let series = self.history_series(q).await?;
        let (text, rows) = series_csv(&series)?;
        let dir = self.exports_dir();
        std::fs::create_dir_all(dir)?;
        let stamp = OffsetDateTime::now_utc().format(time::macros::format_description!(
            "[year][month][day]-[hour][minute][second]"
        ))?;
        let stem = format!("history-{}-{}-{stamp}", q.metric.slug(), q.group.slug());
        let mut path = dir.join(format!("{stem}.csv"));
        let mut n = 2;
        while path.exists() {
            path = dir.join(format!("{stem}-{n}.csv"));
            n += 1;
        }
        std::fs::write(&path, text)?;
        Ok(SavedCsv { path, rows })
    }

    pub(crate) async fn title_order(&self) -> Result<HashMap<i64, i64>> {
        self.sort_positions(crate::storage::BookSort::Title).await
    }

    pub(crate) async fn sort_positions(
        &self,
        sort: crate::storage::BookSort,
    ) -> Result<HashMap<i64, i64>> {
        Ok(self
            .storage
            .list_books(&BookQuery::new(-1, sort))
            .await?
            .into_iter()
            .enumerate()
            .filter_map(|(i, b)| Some((b.id?, i as i64)))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Date {
        parse_day(s).unwrap()
    }

    #[test]
    fn weeks_start_on_monday_and_months_on_the_first() {
        assert_eq!(
            HistoryGroup::Week.period_of(d("2026-11-10")),
            d("2026-11-09")
        );
        assert_eq!(
            HistoryGroup::Week.period_of(d("2026-11-09")),
            d("2026-11-09")
        );
        assert_eq!(
            HistoryGroup::Week.period_of(d("2026-11-15")),
            d("2026-11-09")
        );
        assert_eq!(
            HistoryGroup::Month.period_of(d("2026-02-28")),
            d("2026-02-01")
        );
        assert_eq!(last_of_month(d("2024-02-03")), d("2024-02-29"));
        assert_eq!(year_before(d("2024-02-29")), d("2023-02-28"));
    }

    #[test]
    fn a_span_reads_at_day_grain_and_stops_at_today() {
        let span = HistorySpan::new(
            Some(midnight(d("2026-01-01"))),
            Some(midnight(d("2027-01-01"))),
        )
        .unwrap();
        assert_eq!(
            span.days(None, d("2026-11-10")),
            Some((d("2026-01-01"), d("2026-11-10")))
        );
        assert!(HistorySpan::new(Some(5), Some(5)).is_err());
        assert_eq!(HistorySpan::default().days(None, d("2026-11-10")), None);
    }

    #[test]
    fn a_book_title_cannot_become_a_formula_in_the_csv() {
        let series = HistorySeries {
            buckets: vec![SeriesBucket {
                period: "2026-01-05".into(),
                start: "2026-01-05".into(),
                end: "2026-01-11".into(),
                value: 3,
                known: true,
                parts: vec![SeriesPart {
                    key: "7".into(),
                    label: r#"=HYPERLINK("https://example.invalid","click")"#.into(),
                    value: 3,
                }],
                compare: None,
            }],
            total: 3,
            known: true,
            average: Some(3.0),
            busiest: Some(0),
            compare_total: None,
            compare_change: None,
        };
        let (text, rows) = series_csv(&series).unwrap();
        assert_eq!(rows, 2);
        let mut reader = csv::Reader::from_reader(text.as_bytes());
        let part = reader.records().nth(1).unwrap().unwrap();
        assert_eq!(
            &part[4],
            r#"'=HYPERLINK("https://example.invalid","click")"#
        );
    }

    #[test]
    fn a_run_still_going_is_never_reported_even_clipped() {
        let ev = |day: &str| DayFact {
            book_id: 1,
            day: day.into(),
            minutes: None,
            pages: None,
            source: "vault".into(),
        };
        let mut events = Vec::new();
        let mut day = d("2025-12-30");
        while day <= d("2026-11-09") {
            events.push(ev(&fmt_day(day)));
            day += Duration::days(1);
        }
        events.push(ev("2025-06-01"));
        events.push(ev("2025-06-02"));
        let now = midnight(d("2026-11-10")) + 3600;
        let y2025 = HistorySpan::new(
            Some(midnight(d("2025-01-01"))),
            Some(midnight(d("2026-01-01"))),
        )
        .unwrap();
        let run = longest_ended_run(&events, &y2025, now).unwrap();
        assert_eq!(
            (run.days, run.first.as_str()),
            (2, "2025-06-01"),
            "the run still going is excluded whole, not clipped to 31 Dec"
        );
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    fn point() -> impl Strategy<Value = DayPoint> {
        (
            0i64..120,
            proptest::option::of(1i64..5),
            0usize..3,
            0i64..50,
        )
            .prop_map(|(off, book, src, value)| DayPoint {
                day: parse_day("2025-12-01").unwrap() + Duration::days(off),
                book_id: book,
                source: ["koreader", "local", "vault"][src].to_string(),
                value,
            })
    }

    fn group() -> impl Strategy<Value = HistoryGroup> {
        prop_oneof![
            Just(HistoryGroup::Day),
            Just(HistoryGroup::Week),
            Just(HistoryGroup::Month)
        ]
    }

    fn query(group: HistoryGroup, split: HistorySplit) -> SeriesQuery {
        SeriesQuery {
            metric: HistoryMetric::Passages,
            group,
            split,
            compare: HistoryCompare::Nothing,
            span: HistorySpan::default(),
        }
    }

    proptest! {
        #[test]
        fn groupings_partition_the_days(a in 0i64..400, len in 0i64..400, g in group()) {
            let first = parse_day("2025-01-01").unwrap() + Duration::days(a);
            let last = first + Duration::days(len);
            let b = buckets_of(g, first, last);
            prop_assert_eq!(b[0].1, first);
            prop_assert_eq!(b[b.len() - 1].2, last);
            for w in b.windows(2) {
                prop_assert_eq!(w[0].2 + Duration::days(1), w[1].1);
            }
            for (period, start, end) in &b {
                prop_assert!(start <= end);
                prop_assert_eq!(g.period_of(*start), *period);
                prop_assert_eq!(g.period_of(*end), *period);
            }
        }

        #[test]
        fn a_series_sums_to_its_total(pts in proptest::collection::vec(point(), 0..80),
                                      a in 0i64..60, len in 0i64..90, g in group()) {
            let first = parse_day("2025-12-01").unwrap() + Duration::days(a);
            let last = first + Duration::days(len);
            let s = series(&pts, &query(g, HistorySplit::Unsplit), Some((first, last)), &HashMap::new());
            let inside: i64 = pts.iter().filter(|p| p.day >= first && p.day <= last).map(|p| p.value).sum();
            prop_assert_eq!(s.buckets.iter().map(|b| b.value).sum::<i64>(), s.total);
            prop_assert_eq!(s.total, inside);
        }

        #[test]
        fn a_splits_parts_sum_to_the_unsplit_series(pts in proptest::collection::vec(point(), 0..80),
                                                    g in group(), by_book in any::<bool>()) {
            let days = Some((parse_day("2025-12-01").unwrap(), parse_day("2026-03-30").unwrap()));
            let split = if by_book { HistorySplit::Book } else { HistorySplit::Device };
            let whole = series(&pts, &query(g, HistorySplit::Unsplit), days, &HashMap::new());
            let parted = series(&pts, &query(g, split), days, &HashMap::new());
            prop_assert_eq!(whole.buckets.len(), parted.buckets.len());
            for (w, p) in whole.buckets.iter().zip(&parted.buckets) {
                prop_assert_eq!(w.value, p.value);
                prop_assert_eq!(p.parts.iter().map(|x| x.value).sum::<i64>(), w.value);
            }
        }

        #[test]
        fn note_attribution_is_order_independent(
            wins in proptest::collection::vec((1i64..4, 0i64..100, 0i64..100), 0..8),
            at in 0i64..100, book in 1i64..4,
        ) {
            let windows: Vec<ReadWindow> = wins.iter().enumerate().map(|(i, &(b, x, y))| ReadWindow {
                reading_id: i as i64 + 1, book_id: b, start: x.min(y), end: x.max(y),
            }).collect();
            let note = MarkStamp { id: 1, book_id: Some(book), reading_id: None, at,
                                   source: "vault".into(), kind: "note".into() };
            let forward = attribute_note(&note, &windows);
            let mut rev = windows.clone();
            rev.reverse();
            prop_assert_eq!(forward, attribute_note(&note, &rev));
            if let Some(r) = forward {
                let w = windows.iter().find(|w| w.reading_id == r).unwrap();
                prop_assert!(w.book_id == book && w.start <= at && at <= w.end);
            }
        }
    }
}
