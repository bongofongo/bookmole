//! The Kindle's `My Clippings.txt`, imported one-way as highlights. The file is an append-only
//! log, so nothing is removed on import; a title that matches no book creates none.
//! A last block without its closing `==========` is an unfinished write: counted unparsed.
//! `made_at` is the device's local time; the file carries no zone, so none is applied.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::copies::{CopyForm, LearnedFrom};
use crate::diagnostic::{Diagnostic, ErrorClass};
use crate::error::{EngineError, Result};
use crate::highlight_origin::{
    HighlightImportSummary, HighlightOrigin, ImportedHighlight, ImportedOutcome, PosKind,
};
use crate::koreader::{MatchCandidate, band, scores_for};
use crate::matching::Query;
use crate::storage::Storage;

pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 200_000;
/// The longest text one entry may carry. The Kindle's own clipping limit keeps
/// real entries far below it; a longer one is counted `unparsed`, not cut.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const SEPARATOR: &str = "==========";
/// Highlight visits note pairing may make in one title; past it the remaining notes stay note-only
/// rather than stall the import.
const MAX_PAIRING_WORK: usize = 50_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateLayout {
    MonthWord,
}

/// One language's words for meta lines. Lowercase, compared case-insensitively; each list holds
/// every spelling firmware has used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindleWords {
    pub language: &'static str,
    pub your: &'static [&'static str],
    pub highlight: &'static [&'static str],
    pub note: &'static [&'static str],
    pub bookmark: &'static [&'static str],
    pub on: &'static [&'static str],
    pub page: &'static [&'static str],
    pub location: &'static [&'static str],
    pub added_on: &'static [&'static str],
    pub months: [&'static str; 12],
    pub weekdays: [&'static str; 7],
    pub am: &'static str,
    pub pm: &'static str,
    pub date_layout: DateLayout,
}

pub const ENGLISH: KindleWords = KindleWords {
    language: "en",
    your: &["your"],
    highlight: &["highlight"],
    note: &["note"],
    bookmark: &["bookmark"],
    on: &["on"],
    page: &["page"],
    location: &["location", "loc."],
    added_on: &["added on"],
    months: [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ],
    weekdays: [
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ],
    am: "am",
    pm: "pm",
    date_layout: DateLayout::MonthWord,
};

/// Every language a line can be read in, tried in order. English only; CJK is not handled.
pub const LANGUAGES: &[KindleWords] = &[ENGLISH];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindleEntryKind {
    Highlight,
    Note,
    Bookmark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindleLocation {
    pub start: u64,
    /// The range's end exactly as written (old firmware abbreviates
    /// `466-469` to `466-69`).
    pub end: Option<u64>,
}

impl KindleLocation {
    pub fn as_given(&self) -> String {
        match self.end {
            Some(e) => format!("{}-{e}", self.start),
            None => self.start.to_string(),
        }
    }

    /// The end as a location: an abbreviated end (`466-69`) takes the start's
    /// leading digits. Used only to pair notes, never stored.
    fn full_end(&self) -> u64 {
        let Some(end) = self.end else {
            return self.start;
        };
        if end >= self.start {
            return end;
        }
        let digits = end.checked_ilog10().map_or(1, |d| d + 1);
        let Some(p) = 10u64.checked_pow(digits) else {
            return self.start;
        };
        let mut full = self.start - self.start % p + end;
        if full < self.start {
            full = full.saturating_add(p);
        }
        full
    }
}

pub(crate) fn parse_location(s: &str) -> Option<KindleLocation> {
    let digits = |t: &str| {
        (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
            .then(|| t.parse::<u64>().ok())
            .flatten()
    };
    match s.split_once('-') {
        Some((a, b)) => Some(KindleLocation {
            start: digits(a.trim())?,
            end: Some(digits(b.trim())?),
        }),
        None => Some(KindleLocation {
            start: digits(s.trim())?,
            end: None,
        }),
    }
}

/// The leading page number of `12` or `12-13`; `None` for a roman or other
/// non-numeric page, which is kept only as text.
pub(crate) fn page_number(page: &str) -> Option<i64> {
    let head = page.split('-').next()?.trim();
    (!head.is_empty() && head.bytes().all(|b| b.is_ascii_digit()))
        .then(|| head.parse().ok())
        .flatten()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClippingEntry {
    pub title_line: String,
    pub kind: KindleEntryKind,
    pub page: Option<String>,
    pub location: Option<KindleLocation>,
    /// `YYYY-MM-DD HH:MM:SS`, the device's local time (no zone in the file).
    pub added_on: Option<String>,
    /// Lines joined by `\n`, ends trimmed. Empty for a bookmark.
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClippingsParse {
    pub entries: Vec<ClippingEntry>,
    /// Entries seen and not read: no language row matched, structurally
    /// broken, too long, past [`MAX_ENTRIES`], or the unfinished last write.
    pub unparsed: usize,
}

pub(crate) fn strip_word<'a>(s: &'a str, words: &[&str]) -> Option<&'a str> {
    'words: for w in words {
        let mut chars = s.char_indices();
        let mut end = 0;
        for wc in w.chars() {
            match chars.next() {
                Some((i, c)) if c.to_lowercase().eq(wc.to_lowercase()) => end = i + c.len_utf8(),
                _ => continue 'words,
            }
        }
        let rest = &s[end..];
        let boundary = rest.chars().next().is_none_or(|c| !c.is_alphanumeric())
            || w.ends_with(|c: char| !c.is_alphanumeric());
        if boundary {
            return Some(rest);
        }
    }
    None
}

#[derive(Debug, Default)]
struct Meta {
    kind: Option<KindleEntryKind>,
    page: Option<String>,
    location: Option<KindleLocation>,
    added_on: Option<String>,
}

pub(crate) fn kind_word<'a>(s: &'a str, w: &KindleWords) -> Option<(KindleEntryKind, &'a str)> {
    let s = strip_word(s, w.your).map_or(s, str::trim_start);
    if let Some(r) = strip_word(s, w.highlight) {
        Some((KindleEntryKind::Highlight, r))
    } else if let Some(r) = strip_word(s, w.note) {
        Some((KindleEntryKind::Note, r))
    } else {
        strip_word(s, w.bookmark).map(|r| (KindleEntryKind::Bookmark, r))
    }
}

fn position(meta: &mut Meta, seg: &str, w: &KindleWords) -> Option<()> {
    if let Some(v) = strip_word(seg, w.page) {
        let v = v.trim();
        if v.is_empty() || v.contains(char::is_whitespace) || meta.page.is_some() {
            return None;
        }
        meta.page = Some(v.to_string());
        return Some(());
    }
    let v = strip_word(seg, w.location)?;
    if meta.location.is_some() {
        return None;
    }
    meta.location = Some(parse_location(v.trim())?);
    Some(())
}

fn parse_meta(line: &str, w: &KindleWords) -> Option<Meta> {
    let line = line.trim().trim_start_matches('\u{feff}');
    let line = line.strip_prefix('-')?.trim_start();
    let mut segs = line.split('|').map(str::trim);
    let (kind, rest) = kind_word(segs.next()?, w)?;
    let mut meta = Meta {
        kind: Some(kind),
        ..Meta::default()
    };
    let rest = rest.trim();
    let rest = strip_word(rest, w.on).map_or(rest, str::trim);
    if !rest.is_empty() {
        position(&mut meta, rest, w)?;
    }
    for seg in segs {
        if seg.is_empty() {
            continue;
        }
        if let Some(d) = strip_word(seg, w.added_on) {
            if meta.added_on.is_some() {
                return None;
            }
            meta.added_on = Some(parse_date(d.trim(), w)?);
        } else {
            position(&mut meta, seg, w)?;
        }
    }
    Some(meta)
}

/// A date in `w`'s layout, as `YYYY-MM-DD HH:MM:SS`. `None` when anything in
/// it is not the row's, so another row (or `unparsed`) gets the entry.
pub(crate) fn parse_date(s: &str, w: &KindleWords) -> Option<String> {
    match w.date_layout {
        DateLayout::MonthWord => parse_month_word_date(s, w),
    }
}

fn parse_month_word_date(s: &str, w: &KindleWords) -> Option<String> {
    let (mut month, mut day, mut year, mut clock, mut pm) = (None, None, None, None, None);
    for tok in s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
    {
        let low = tok.to_lowercase();
        if let Some(i) = w.months.iter().position(|m| *m == low) {
            if month.replace(i as u8 + 1).is_some() {
                return None;
            }
        } else if w.weekdays.contains(&low.as_str()) {
            continue;
        } else if low == w.am || low == w.pm {
            if pm.replace(low == w.pm).is_some() {
                return None;
            }
        } else if tok.contains(':') {
            let parts: Vec<&str> = tok.split(':').collect();
            if !(2..=3).contains(&parts.len())
                || parts
                    .iter()
                    .any(|p| p.is_empty() || p.len() > 2 || !p.bytes().all(|b| b.is_ascii_digit()))
            {
                return None;
            }
            let n = |i: usize| parts.get(i).map_or(Some(0), |p| p.parse::<u8>().ok());
            if clock.replace((n(0)?, n(1)?, n(2)?)).is_some() {
                return None;
            }
        } else if tok.bytes().all(|b| b.is_ascii_digit()) {
            match tok.len() {
                4 => {
                    if year.replace(tok.parse::<i32>().ok()?).is_some() {
                        return None;
                    }
                }
                1 | 2 => {
                    if day.replace(tok.parse::<u8>().ok()?).is_some() {
                        return None;
                    }
                }
                _ => return None,
            }
        } else {
            return None;
        }
    }
    let (mut h, mi, se) = clock?;
    if let Some(pm) = pm {
        if !(1..=12).contains(&h) {
            return None;
        }
        h = h % 12 + if pm { 12 } else { 0 };
    }
    let month = time::Month::try_from(month?).ok()?;
    let date = time::Date::from_calendar_date(year?, month, day?).ok()?;
    let at = time::Time::from_hms(h, mi, se).ok()?;
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        date.year(),
        u8::from(date.month()),
        date.day(),
        at.hour(),
        at.minute(),
        at.second()
    ))
}

fn clean_title(line: &str) -> &str {
    line.trim().trim_start_matches('\u{feff}').trim()
}

/// `Title (Author)` split at the **last** parenthesised group. Several authors
/// are `;`-separated. A line with no trailing group is all title.
pub fn split_title_line(line: &str) -> (String, Vec<String>) {
    let t = clean_title(line);
    if t.ends_with(')') {
        let mut depth = 0usize;
        for (i, c) in t.char_indices().rev() {
            match c {
                ')' => depth += 1,
                '(' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let title = t[..i].trim();
                        if title.is_empty() {
                            break;
                        }
                        let authors = t[i + 1..t.len() - 1]
                            .split(';')
                            .map(str::trim)
                            .filter(|a| !a.is_empty())
                            .map(str::to_string)
                            .collect();
                        return (title.to_string(), authors);
                    }
                }
                _ => {}
            }
        }
    }
    (t.to_string(), Vec::new())
}

/// Pure, never panics, no I/O: the property and fuzz surface.
pub fn parse_clippings(text: &str) -> ClippingsParse {
    let mut out = ClippingsParse::default();
    let mut block: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim_start_matches('\u{feff}').trim() == SEPARATOR {
            finish_block(&block, &mut out);
            block.clear();
        } else {
            block.push(line);
        }
    }
    // Whatever follows the last separator was not finished by the device.
    if block.iter().any(|l| !clean_title(l).is_empty()) {
        out.unparsed += 1;
    }
    out
}

fn finish_block(block: &[&str], out: &mut ClippingsParse) {
    let mut lines = block
        .iter()
        .copied()
        .skip_while(|l| clean_title(l).is_empty());
    let Some(title) = lines.next() else {
        return;
    };
    if out.entries.len() >= MAX_ENTRIES {
        out.unparsed += 1;
        return;
    }
    let title = clean_title(title);
    let Some(meta_line) = lines.next() else {
        out.unparsed += 1;
        return;
    };
    let rest: Vec<&str> = lines.collect();
    let body = match rest.split_first() {
        Some((first, tail)) if first.trim().is_empty() => tail,
        _ => &rest[..],
    };
    let text = body.join("\n");
    let text = text.trim();
    let Some(meta) = LANGUAGES.iter().find_map(|w| parse_meta(meta_line, w)) else {
        out.unparsed += 1;
        return;
    };
    let kind = meta.kind.unwrap_or(KindleEntryKind::Highlight);
    if text.len() > MAX_TEXT_BYTES || (kind == KindleEntryKind::Note && text.is_empty()) {
        out.unparsed += 1;
        return;
    }
    out.entries.push(ClippingEntry {
        title_line: title.to_string(),
        kind,
        page: meta.page,
        location: meta.location,
        added_on: meta.added_on,
        text: text.to_string(),
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClippingsBook {
    pub title_line: String,
    pub title: String,
    pub authors: Vec<String>,
    pub highlights: Vec<ImportedHighlight>,
    pub bookmarks: usize,
}

/// Where a highlight or note sits, for pairing: a location range, else a page
/// range. Never both, never compared across units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Spot {
    Location(u64),
    Page(i64),
}

type Row = (usize, ImportedHighlight, Option<(Spot, Spot)>);

fn spot_range(e: &ClippingEntry) -> Option<(Spot, Spot)> {
    if let Some(l) = e.location {
        return Some((Spot::Location(l.start), Spot::Location(l.full_end())));
    }
    let page = e.page.as_deref()?;
    let start = page_number(page)?;
    let end = page
        .split_once('-')
        .and_then(|(_, b)| page_number(b))
        .filter(|e| *e >= start)
        .unwrap_or(start);
    Some((Spot::Page(start), Spot::Page(end)))
}

fn position_key(e: &ClippingEntry) -> String {
    match (&e.location, &e.page) {
        (Some(l), _) => l.as_given(),
        (None, Some(p)) => format!("p{p}"),
        (None, None) => String::new(),
    }
}

fn base_highlight(e: &ClippingEntry, identity: String, text: String) -> ImportedHighlight {
    let mut h = ImportedHighlight::new(HighlightOrigin::Kindle, identity, text);
    h.page = e.page.as_deref().and_then(page_number);
    if let Some(l) = e.location {
        h.pos_kind = Some(PosKind::KindleLocation);
        h.pos0 = Some(l.start.to_string());
        h.pos1 = l.end.map(|e| e.to_string());
    }
    h.made_at = e.added_on.clone();
    h
}

/// Groups a parse by title line (first-appearance order) into highlights: notes paired, edits
/// resolved, duplicates collapsed. `copy_id` is left `None`; the import fills it.
pub fn clippings_books(parse: &ClippingsParse) -> Vec<ClippingsBook> {
    let mut order: Vec<&str> = Vec::new();
    let mut groups: HashMap<&str, Vec<&ClippingEntry>> = HashMap::new();
    for e in &parse.entries {
        groups
            .entry(e.title_line.as_str())
            .or_insert_with(|| {
                order.push(e.title_line.as_str());
                Vec::new()
            })
            .push(e);
    }
    order.into_iter().map(|t| book_of(t, &groups[t])).collect()
}

fn book_of(title_line: &str, entries: &[&ClippingEntry]) -> ClippingsBook {
    let (title, authors) = split_title_line(title_line);
    let bookmarks = entries
        .iter()
        .filter(|e| e.kind == KindleEntryKind::Bookmark)
        .count();

    let mut rows: Vec<Row> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (i, e) in entries.iter().enumerate() {
        if e.kind != KindleEntryKind::Highlight {
            continue;
        }
        let identity = format!("{title_line}\u{1f}{}\u{1f}{}", position_key(e), e.text);
        if !seen.insert(identity.clone()) {
            continue;
        }
        let mut h = base_highlight(e, identity, e.text.clone());
        h.text_unavailable = e.text.is_empty();
        rows.push((i, h, spot_range(e)));
    }

    let mut paired: BTreeMap<(usize, Option<Spot>), String> = BTreeMap::new();
    let mut alone: Vec<(String, usize)> = Vec::new();
    let mut alone_at: HashMap<String, usize> = HashMap::new();
    let mut work = MAX_PAIRING_WORK;
    for (i, e) in entries.iter().enumerate() {
        if e.kind != KindleEntryKind::Note {
            continue;
        }
        let spot = spot_range(e).map(|(s, _)| s);
        // Each note looks at every highlight of its title; past the budget
        // (a hostile file only) the rest stay note-only rather than stall.
        let affordable = work >= rows.len();
        work = work.saturating_sub(rows.len());
        let host = spot.filter(|_| affordable).and_then(|s| {
            let containing: Vec<(usize, Spot)> = rows
                .iter()
                .enumerate()
                .filter_map(|(r, (_, _, range))| {
                    let (a, b) = (*range)?;
                    (a <= s && s <= b).then_some((r, a))
                })
                .collect();
            let nearest = containing.iter().map(|(_, a)| *a).max()?;
            let tied: Vec<usize> = containing
                .iter()
                .filter(|(_, a)| *a == nearest)
                .map(|(r, _)| *r)
                .collect();
            tied.iter()
                .copied()
                .filter(|r| rows[*r].0 < i)
                .max_by_key(|r| rows[*r].0)
                .or_else(|| {
                    tied.iter()
                        .copied()
                        .filter(|r| rows[*r].0 > i)
                        .min_by_key(|r| rows[*r].0)
                })
        });
        match host {
            Some(r) => {
                paired.insert((r, spot), e.text.clone());
            }
            None => {
                let key = format!("{title_line}\u{1f}note\u{1f}{}", position_key(e));
                match alone_at.get(&key) {
                    Some(&slot) => alone[slot].1 = i,
                    None => {
                        alone_at.insert(key.clone(), alone.len());
                        alone.push((key, i));
                    }
                }
            }
        }
    }
    let mut notes: HashMap<usize, Vec<String>> = HashMap::new();
    for ((r, _), text) in paired {
        notes.entry(r).or_default().push(text);
    }

    let mut out: Vec<(usize, ImportedHighlight)> = rows
        .into_iter()
        .enumerate()
        .map(|(r, (i, mut h, _))| {
            if let Some(n) = notes.remove(&r) {
                h.origin_note = Some(n.join("\n\n"));
            }
            (i, h)
        })
        .collect();
    for (key, i) in alone {
        let e = entries[i];
        let mut h = base_highlight(e, key, String::new());
        h.origin_note = Some(e.text.clone());
        out.push((i, h));
    }
    out.sort_by_key(|(i, _)| *i);
    ClippingsBook {
        title_line: title_line.to_string(),
        title,
        authors,
        highlights: out.into_iter().map(|(_, h)| h).collect(),
        bookmarks,
    }
}

/// Read a text file of at most `cap` bytes. `Err` is the `(class, detail)` of
/// why not: missing, not a file, too large, binary (a NUL byte) or not UTF-8.
pub(crate) fn read_text_capped(
    path: &Path,
    cap: u64,
) -> std::result::Result<String, (ErrorClass, String)> {
    use std::io::Read;
    let meta = std::fs::metadata(path)
        .map_err(|e| (ErrorClass::Io, format!("cannot read: {}", e.kind())))?;
    if !meta.is_file() {
        return Err((ErrorClass::Io, "not a file".into()));
    }
    if meta.len() > cap {
        return Err((ErrorClass::Other, "too large to read".into()));
    }
    let file = std::fs::File::open(path)
        .map_err(|e| (ErrorClass::Io, format!("cannot read: {}", e.kind())))?;
    let mut bytes = Vec::new();
    // The file may grow between the stat and the read (a device still writing).
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| (ErrorClass::Io, format!("cannot read: {}", e.kind())))?;
    if bytes.len() as u64 > cap {
        return Err((ErrorClass::Other, "too large to read".into()));
    }
    if bytes.contains(&0) {
        return Err((ErrorClass::Decode, "not a text file".into()));
    }
    String::from_utf8(bytes).map_err(|_| (ErrorClass::Decode, "not UTF-8 text".into()))
}

/// `documents/My Clippings.txt` on a mounted Kindle, matched case-insensitively; `None` if absent.
pub fn clippings_path_on(mount: &Path) -> Option<PathBuf> {
    let docs = child_ci(mount, "documents", true)?;
    child_ci(&docs, "My Clippings.txt", false)
}

fn child_ci(dir: &Path, name: &str, want_dir: bool) -> Option<PathBuf> {
    let is = |p: &Path| if want_dir { p.is_dir() } else { p.is_file() };
    let exact = dir.join(name);
    if is(&exact) {
        return Some(exact);
    }
    let mut hits: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
        .map(|e| e.path())
        .filter(|p| is(p))
        .collect();
    hits.sort();
    hits.into_iter().next()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindleClippingsBook {
    pub title_line: String,
    pub book_id: i64,
    pub copy_id: Option<i64>,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnmatchedKindleTitle {
    pub title_line: String,
    pub title: String,
    pub authors: Vec<String>,
    pub entries: usize,
    pub candidates: Vec<MatchCandidate>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct KindleClippingsReport {
    pub dry_run: bool,
    pub books: Vec<KindleClippingsBook>,
    pub unmatched: Vec<UnmatchedKindleTitle>,
    pub bookmarks: usize,
    /// Totals over `books`, plus `unparsed` and every diagnostic. `removed` is always 0.
    pub summary: HighlightImportSummary,
}

/// The one copy of `book_id` that is `form = app` with app `kindle`, or learned from a Kindle;
/// `None` if there is none or several. Never creates a copy.
pub async fn kindle_copy(storage: &Storage, book_id: i64) -> Result<Option<i64>> {
    let fits: Vec<i64> = storage
        .copies_for_book(book_id)
        .await?
        .into_iter()
        .filter(|c| {
            let app = c.form == Some(CopyForm::App)
                && c.app
                    .as_deref()
                    .is_some_and(|a| a.trim().eq_ignore_ascii_case("kindle"));
            app || c.learned_from == LearnedFrom::Kindle
        })
        .map(|c| c.id)
        .collect();
    Ok(match fits.as_slice() {
        [id] => Some(*id),
        _ => None,
    })
}

pub(crate) async fn store_highlights(
    storage: &Storage,
    book_id: i64,
    copy_id: Option<i64>,
    highlights: Vec<ImportedHighlight>,
    dry_run: bool,
) -> Result<(usize, usize, usize)> {
    let (mut inserted, mut updated, mut unchanged) = (0, 0, 0);
    for mut h in highlights {
        h.copy_id = copy_id;
        let outcome = if dry_run {
            storage.preview_imported_highlight(book_id, &h).await?
        } else {
            Some(storage.upsert_imported_highlight(book_id, &h).await?)
        };
        match outcome {
            None | Some(ImportedOutcome::Inserted(_)) => inserted += 1,
            Some(ImportedOutcome::Updated(_)) => updated += 1,
            Some(ImportedOutcome::Unchanged(_)) => unchanged += 1,
        }
    }
    Ok((inserted, updated, unchanged))
}

/// Matches each title through the shared matcher (auto band only); an unmatched title creates no
/// book. An unreadable file is `Ok` with a diagnostic; `Err` is a storage failure.
pub async fn import_kindle_clippings(
    storage: &Storage,
    path: &Path,
    dry_run: bool,
) -> Result<KindleClippingsReport> {
    import_file(storage, path, None, dry_run).await
}

/// Imports only entries whose title line is `title_line` into `book_id`, without matching.
/// `NotFound` when the book does not exist.
pub async fn import_kindle_clippings_into(
    storage: &Storage,
    path: &Path,
    title_line: &str,
    book_id: i64,
    dry_run: bool,
) -> Result<KindleClippingsReport> {
    if storage.get_book(book_id).await?.is_none() {
        return Err(EngineError::NotFound(format!("book id {book_id}")));
    }
    import_file(storage, path, Some((title_line, book_id)), dry_run).await
}

async fn import_file(
    storage: &Storage,
    path: &Path,
    only: Option<(&str, i64)>,
    dry_run: bool,
) -> Result<KindleClippingsReport> {
    let owned = path.to_path_buf();
    let read = tokio::task::spawn_blocking(move || read_text_capped(&owned, MAX_FILE_BYTES))
        .await
        .map_err(|e| EngineError::Other(format!("clippings read task: {e}")))?;
    let text = match read {
        Ok(t) => t,
        Err((class, detail)) => {
            let mut report = KindleClippingsReport {
                dry_run,
                ..KindleClippingsReport::default()
            };
            report
                .summary
                .diagnostics
                .push(Diagnostic::kindle_clippings_unreadable(
                    path.to_path_buf(),
                    class,
                    detail,
                ));
            return Ok(report);
        }
    };
    import_parsed(storage, path, &parse_clippings(&text), only, dry_run).await
}

pub(crate) async fn import_parsed(
    storage: &Storage,
    path: &Path,
    parse: &ClippingsParse,
    only: Option<(&str, i64)>,
    dry_run: bool,
) -> Result<KindleClippingsReport> {
    let mut report = KindleClippingsReport {
        dry_run,
        ..KindleClippingsReport::default()
    };
    report.summary.unparsed = parse.unparsed;
    let wanted = only.map(|(t, id)| (clean_title(t), id));
    for book in clippings_books(parse) {
        let book_id = match wanted {
            Some((t, id)) if t == book.title_line => id,
            Some(_) => continue,
            None => {
                let scored =
                    scores_for(storage, &Query::new(Some(&book.title), &book.authors)).await?;
                match scored
                    .first()
                    .filter(|s| s.can_auto)
                    .and_then(|s| s.book.id)
                {
                    Some(id) => id,
                    None => {
                        let entries = book.highlights.len();
                        report
                            .summary
                            .diagnostics
                            .push(Diagnostic::kindle_title_unmatched(&book.title, entries));
                        report.unmatched.push(UnmatchedKindleTitle {
                            title_line: book.title_line,
                            title: book.title,
                            authors: book.authors,
                            entries,
                            candidates: band(scored),
                        });
                        continue;
                    }
                }
            }
        };
        let copy_id = kindle_copy(storage, book_id).await?;
        let (inserted, updated, unchanged) =
            store_highlights(storage, book_id, copy_id, book.highlights, dry_run).await?;
        report.summary.inserted += inserted;
        report.summary.updated += updated;
        report.summary.unchanged += unchanged;
        report.bookmarks += book.bookmarks;
        report.books.push(KindleClippingsBook {
            title_line: book.title_line,
            book_id,
            copy_id,
            inserted,
            updated,
            unchanged,
        });
    }
    if parse.unparsed > 0 {
        report
            .summary
            .diagnostics
            .push(Diagnostic::kindle_entries_unparsed(
                path.to_path_buf(),
                parse.unparsed,
            ));
    }
    tracing::debug!(
        books = report.books.len(),
        unmatched = report.unmatched.len(),
        inserted = report.summary.inserted,
        updated = report.summary.updated,
        unchanged = report.summary.unchanged,
        unparsed = report.summary.unparsed,
        dry_run,
        "imported kindle clippings"
    );
    Ok(report)
}

/// Writes entries as an English Kindle does (`\r\n`, BOM first, separator after each entry, 12-hour
/// clock); the inverse of [`parse_clippings`] for well-formed English entries.
#[cfg(any(test, feature = "internals"))]
pub fn render_clippings(entries: &[ClippingEntry]) -> String {
    let w = &ENGLISH;
    let cap = |s: &str| {
        let mut c = s.chars();
        c.next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default()
    };
    let mut out = String::from("\u{feff}");
    for e in entries {
        let kind = match e.kind {
            KindleEntryKind::Highlight => "Highlight",
            KindleEntryKind::Note => "Note",
            KindleEntryKind::Bookmark => "Bookmark",
        };
        let mut segs: Vec<String> = Vec::new();
        let mut first = format!("- Your {kind}");
        let mut on = true;
        if let Some(p) = &e.page {
            first.push_str(&format!(" on page {p}"));
            on = false;
        }
        if let Some(l) = &e.location {
            if on {
                first.push_str(&format!(" on Location {}", l.as_given()));
            } else {
                segs.push(format!("Location {}", l.as_given()));
            }
        }
        if let Some(d) = &e.added_on {
            let (date, clock) = d.split_once(' ').unwrap_or((d, "00:00:00"));
            let mut dp = date.split('-').map(|x| x.parse::<u32>().unwrap_or(1));
            let (y, m, dd) = (
                dp.next().unwrap_or(2000),
                dp.next().unwrap_or(1),
                dp.next().unwrap_or(1),
            );
            let mut cp = clock.split(':').map(|x| x.parse::<u32>().unwrap_or(0));
            let (h, mi, s) = (
                cp.next().unwrap_or(0),
                cp.next().unwrap_or(0),
                cp.next().unwrap_or(0),
            );
            let (h12, half) = match h {
                0 => (12, "AM"),
                1..=11 => (h, "AM"),
                12 => (12, "PM"),
                _ => (h - 12, "PM"),
            };
            segs.push(format!(
                "Added on {}, {} {dd}, {y} {h12}:{mi:02}:{s:02} {half}",
                cap(w.weekdays[(y as usize + m as usize + dd as usize) % 7]),
                cap(w.months[(m as usize).clamp(1, 12) - 1]),
            ));
        }
        let mut meta = first;
        for s in segs {
            meta.push_str(" | ");
            meta.push_str(&s);
        }
        out.push_str(&format!(
            "{}\r\n{meta}\r\n\r\n{}\r\n{SEPARATOR}\r\n",
            e.title_line,
            e.text.replace('\n', "\r\n")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(line: &str) -> Option<Meta> {
        parse_meta(line, &ENGLISH)
    }

    #[test]
    fn english_meta_lines_in_their_variants() {
        let m = meta("- Your Highlight on page 12 | Location 180-183 | Added on Monday, March 3, 2025 10:15:22 AM").unwrap();
        assert_eq!(m.kind, Some(KindleEntryKind::Highlight));
        assert_eq!(m.page.as_deref(), Some("12"));
        assert_eq!(
            m.location,
            Some(KindleLocation {
                start: 180,
                end: Some(183)
            })
        );
        assert_eq!(m.added_on.as_deref(), Some("2025-03-03 10:15:22"));

        let m = meta("- Your Note on Location 181 | Added on Monday, March 3, 2025 10:16:00 PM")
            .unwrap();
        assert_eq!(m.kind, Some(KindleEntryKind::Note));
        assert_eq!(
            m.location,
            Some(KindleLocation {
                start: 181,
                end: None
            })
        );
        assert_eq!(m.added_on.as_deref(), Some("2025-03-03 22:16:00"));

        let m =
            meta("- Your Bookmark on Page 12 | Added on Sunday, 2 March 2025 23:05:09").unwrap();
        assert_eq!(m.kind, Some(KindleEntryKind::Bookmark));
        assert_eq!(m.page.as_deref(), Some("12"));
        assert_eq!(m.added_on.as_deref(), Some("2025-03-02 23:05:09"));

        let m =
            meta("- Highlight Loc. 466-69  | Added on Friday, March 30, 2012, 03:49 PM").unwrap();
        assert_eq!(
            m.location,
            Some(KindleLocation {
                start: 466,
                end: Some(69)
            })
        );
        assert_eq!(m.added_on.as_deref(), Some("2012-03-30 15:49:00"));
        assert_eq!(m.location.unwrap().full_end(), 469);

        let m = meta("- Your Highlight on page 1 | Added on Monday, March 3, 2025 12:01:00 AM")
            .unwrap();
        assert_eq!(m.added_on.as_deref(), Some("2025-03-03 00:01:00"));
    }

    #[test]
    fn what_no_row_reads_is_refused() {
        assert!(meta("- Votre surlignement sur la page 12 | emplacement 180-183 | Ajouté le lundi 3 mars 2025 10:15:22").is_none());
        assert!(
            meta("- Your Highlight on page 12 | Added on Blursday, March 3, 2025 10:15:22")
                .is_none()
        );
        assert!(
            meta("- Your Highlight on page 12 | Added on February 30, 2025 10:15:22").is_none()
        );
        assert!(meta("- Your Highlight on Location twelve").is_none());
        assert!(
            meta("Your Highlight on page 12").is_none(),
            "no leading dash"
        );
        assert!(meta("- Your Highlighter on page 12").is_none());
    }

    #[test]
    fn the_author_is_the_last_group() {
        assert_eq!(
            split_title_line(
                "\u{feff}Gödel, Escher, Bach (20th Anniversary) (Hofstadter, Douglas R.)"
            ),
            (
                "Gödel, Escher, Bach (20th Anniversary)".to_string(),
                vec!["Hofstadter, Douglas R.".to_string()]
            )
        );
        assert_eq!(
            split_title_line("Good Omens (Pratchett, Terry;Gaiman, Neil)").1,
            vec!["Pratchett, Terry".to_string(), "Gaiman, Neil".to_string()]
        );
        assert_eq!(
            split_title_line("A Personal Document"),
            ("A Personal Document".into(), vec![])
        );
        assert_eq!(
            split_title_line("(Only Parens)"),
            ("(Only Parens)".into(), vec![])
        );
    }

    fn entry(kind: KindleEntryKind, loc: (u64, Option<u64>), text: &str) -> ClippingEntry {
        ClippingEntry {
            title_line: "T (A)".into(),
            kind,
            page: None,
            location: Some(KindleLocation {
                start: loc.0,
                end: loc.1,
            }),
            added_on: None,
            text: text.into(),
        }
    }

    fn notes_of(entries: Vec<ClippingEntry>) -> Vec<(String, Option<String>)> {
        let parse = ClippingsParse {
            entries,
            unparsed: 0,
        };
        clippings_books(&parse)[0]
            .highlights
            .iter()
            .map(|h| (h.text.clone(), h.origin_note.clone()))
            .collect()
    }

    #[test]
    fn notes_pair_with_the_nearest_containing_highlight() {
        use KindleEntryKind::*;
        let got = notes_of(vec![
            entry(Highlight, (100, Some(200)), "outer"),
            entry(Highlight, (150, Some(160)), "inner"),
            entry(Note, (155, None), "on the inner"),
            entry(Note, (120, None), "on the outer"),
            entry(Note, (999, None), "on nothing"),
        ]);
        assert_eq!(
            got,
            vec![
                ("outer".into(), Some("on the outer".into())),
                ("inner".into(), Some("on the inner".into())),
                (String::new(), Some("on nothing".into())),
            ]
        );
    }

    #[test]
    fn an_edited_note_replaces_and_a_second_place_joins() {
        use KindleEntryKind::*;
        let got = notes_of(vec![
            entry(Highlight, (10, Some(20)), "words"),
            entry(Note, (12, None), "first draft"),
            entry(Note, (18, None), "another thought"),
            entry(Note, (12, None), "second draft"),
            entry(Note, (50, None), "loose"),
            entry(Note, (50, None), "loose, edited"),
        ]);
        assert_eq!(
            got,
            vec![
                (
                    "words".into(),
                    Some("second draft\n\nanother thought".into())
                ),
                (String::new(), Some("loose, edited".into())),
            ]
        );
    }

    #[test]
    fn a_tie_goes_to_the_last_highlight_before_the_note() {
        use KindleEntryKind::*;
        let got = notes_of(vec![
            entry(Highlight, (10, Some(20)), "earlier"),
            entry(Highlight, (10, Some(30)), "later"),
            entry(Note, (15, None), "n"),
            entry(Highlight, (10, Some(25)), "after"),
        ]);
        assert_eq!(got[1], ("later".into(), Some("n".into())));
        assert_eq!(got[0].1, None);
        assert_eq!(got[2].1, None);
    }

    #[test]
    fn the_unfinished_last_write_is_unparsed() {
        let p = parse_clippings("T (A)\n- Your Highlight on page 1\n\nhalf writ");
        assert!(p.entries.is_empty());
        assert_eq!(p.unparsed, 1);
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::book::Book;
    use proptest::prelude::*;

    const BOOKS: [(&str, &str); 4] = [
        ("Kindred", "Butler, Octavia E."),
        ("Middlemarch (Penguin Classics)", "Eliot, George"),
        ("The Remains of the Day", "Ishiguro, Kazuo"),
        ("Gödel, Escher, Bach", "Hofstadter, Douglas"),
    ];

    fn words() -> impl Strategy<Value = String> {
        proptest::collection::vec("[A-Za-z0-9'’,.;!?é]{1,9}", 1..8).prop_map(|w| w.join(" "))
    }

    fn text(lines: std::ops::Range<usize>) -> impl Strategy<Value = String> {
        proptest::collection::vec(words(), lines).prop_map(|l| l.join("\n"))
    }

    fn date() -> impl Strategy<Value = String> {
        (2000i32..2040, 1u8..=12, 1u8..=28, 0u8..24, 0u8..60, 0u8..60)
            .prop_map(|(y, m, d, h, mi, s)| format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}"))
    }

    fn location() -> impl Strategy<Value = KindleLocation> {
        (1u64..20_000, proptest::option::of(0u64..50)).prop_map(|(start, len)| KindleLocation {
            start,
            end: len.map(|l| start + l),
        })
    }

    fn entry() -> impl Strategy<Value = ClippingEntry> {
        (
            0..BOOKS.len(),
            prop_oneof![
                Just(KindleEntryKind::Highlight),
                Just(KindleEntryKind::Note),
                Just(KindleEntryKind::Bookmark)
            ],
            proptest::option::of(prop_oneof!["[1-9][0-9]{0,3}", "[ivx]{1,4}"]),
            proptest::option::of(location()),
            proptest::option::of(date()),
            text(1..4),
            text(0..4),
        )
            .prop_map(|(b, kind, page, location, added_on, some, any)| {
                let (t, a) = BOOKS[b];
                ClippingEntry {
                    title_line: format!("{t} ({a})"),
                    kind,
                    page,
                    location,
                    added_on,
                    text: match kind {
                        KindleEntryKind::Note => some,
                        KindleEntryKind::Highlight => any,
                        KindleEntryKind::Bookmark => String::new(),
                    },
                }
            })
    }

    fn hostile() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just(SEPARATOR.to_string()),
                Just("- Your Highlight on page ".to_string()),
                Just("- Your Note on Location ".to_string()),
                Just(" | Location 1-".to_string()),
                Just(" | Added on Monday, March 3, ".to_string()),
                Just("12:61:99 PM".to_string()),
                Just("\r\n".to_string()),
                Just("\n".to_string()),
                Just("(".to_string()),
                Just(")".to_string()),
                Just("\u{feff}".to_string()),
                "[0-9]{1,25}",
                "\\PC{0,12}",
            ],
            0..60,
        )
        .prop_map(|v| v.concat())
    }

    async fn storage_with_books() -> Storage {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        for (t, a) in BOOKS {
            let b = Book {
                title: Some(t.into()),
                authors: vec![a.into()],
                ..Default::default()
            };
            s.upsert_book(&b, None).await.unwrap();
        }
        s
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    proptest! {
        #[test]
        fn parsing_never_panics(s in "\\PC{0,400}", h in hostile()) {
            let _ = parse_clippings(&s);
            let p = parse_clippings(&h);
            prop_assert!(p.entries.len() <= MAX_ENTRIES);
            let _ = clippings_books(&p);
        }

        #[test]
        fn render_then_parse_is_the_identity(entries in proptest::collection::vec(entry(), 0..20)) {
            let parsed = parse_clippings(&render_clippings(&entries));
            prop_assert_eq!(parsed.unparsed, 0);
            prop_assert_eq!(parsed.entries, entries);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        #[test]
        fn a_second_import_of_the_same_file_inserts_nothing(
            entries in proptest::collection::vec(entry(), 1..25)
        ) {
            rt().block_on(async {
                let s = storage_with_books().await;
                let p = parse_clippings(&render_clippings(&entries));
                let path = Path::new("My Clippings.txt");
                let first = import_parsed(&s, path, &p, None, false).await.unwrap();
                prop_assert!(first.unmatched.is_empty(), "{:?}", first.unmatched);
                let again = import_parsed(&s, path, &p, None, false).await.unwrap();
                prop_assert_eq!(again.summary.inserted, 0);
                prop_assert_eq!(again.summary.updated, 0);
                prop_assert_eq!(again.summary.unchanged, first.summary.inserted);
                Ok(())
            })?;
        }

        #[test]
        fn appending_entries_inserts_exactly_the_appended_ones(
            entries in proptest::collection::vec(entry(), 0..20),
            added in proptest::collection::vec((0..BOOKS.len(), text(1..3), proptest::option::of(date())), 1..10),
        ) {
            let appended: Vec<ClippingEntry> = added
                .into_iter()
                .enumerate()
                .map(|(i, (b, text, added_on))| {
                    let (t, a) = BOOKS[b];
                    ClippingEntry {
                        title_line: format!("{t} ({a})"),
                        kind: KindleEntryKind::Highlight,
                        page: None,
                        location: Some(KindleLocation { start: 100_000 + 10 * i as u64, end: Some(100_002 + 10 * i as u64) }),
                        added_on,
                        text,
                    }
                })
                .collect();
            rt().block_on(async {
                let s = storage_with_books().await;
                let path = Path::new("My Clippings.txt");
                let before = parse_clippings(&render_clippings(&entries));
                import_parsed(&s, path, &before, None, false).await.unwrap();
                let mut all = entries.clone();
                all.extend(appended.iter().cloned());
                let after = parse_clippings(&render_clippings(&all));
                let r = import_parsed(&s, path, &after, None, false).await.unwrap();
                prop_assert_eq!(r.summary.inserted, appended.len());
                prop_assert_eq!(r.summary.updated, 0);
                Ok(())
            })?;
        }
    }
}
