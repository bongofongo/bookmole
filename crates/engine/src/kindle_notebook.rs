//! The Kindle app's **Export Notebook** file, imported as highlights (origin
//! `kindle_app`).
//!
//! The file is one the user exported and chose by hand, one book per file.
//! Nothing here reads the Kindle app's private storage, and nothing ever needs
//! an Amazon login. Import is one-way, through the shared path
//! [`Storage::upsert_imported_highlight`]; `annotation` is never touched.
//!
//! ## The file, and how it is read
//!
//! The body is a flat run of `div`s told apart by class: `bookTitle`,
//! `authors`, `citation` (ignored), `sectionHeading` (the chapter), then
//! repeating `noteHeading` + `noteText`. A heading reads like
//! `Highlight(<span class="highlight_yellow">yellow</span>) - Chapter 3 > Page
//! 41 · Location 612`, `Note - Page 41 · Location 613` or `Bookmark - …`.
//!
//! **A small tolerant scanner, not an HTML parser.** The engine has no HTML or
//! XML parser among its direct dependencies and this item adds none; the
//! export is not well-formed enough for an XML parser anyway (it has been seen
//! closing a `noteText` with `</h3>`). The scanner walks tags, skipping
//! comments, `<!…>`/`<?…>` declarations and `style`/`script` bodies, and takes
//! a `div` whose `class` (any quoting, any attribute order, any case, among
//! other classes) is one of the five above. Its content runs to the next
//! `<div`, `</div` or `</h3`, whichever is first, which is exactly as much as a
//! flat run of divs needs and survives a missing close tag. Inside it: `<br>`
//! is a line break, other tags are dropped, raw whitespace collapses, entities
//! are decoded (`&amp;`, `&#8217;`, `&#x2019;`, `&nbsp;` and the usual named
//! few; an unknown one is kept as written), and each line is trimmed.
//!
//! ## Headings, through the language table
//!
//! The heading's kind word, page word and location word are read through the
//! same [`LANGUAGES`] table as the clippings (one [`KindleWords`] row per
//! language, English only). After the kind: an optional `(colour)` (the span's
//! `highlight_<colour>` class wins over the word), an optional dash, then
//! pieces split on `·`, `>` and `|`. Pieces are read **from the end** while
//! they are a page (`Page 41`, `Page xii`, `Page 41-42`) or a location
//! (`Location 612`); the first piece that is neither is the heading's chapter
//! path and is ignored (`sectionHeading` is the chapter). A heading whose kind
//! no row knows is counted `unparsed` with its text, and reported once per file
//! as [`DiagnosticKind::KindleEntriesUnparsed`]; so is a note with no words or
//! a text past [`MAX_TEXT_BYTES`].
//!
//! ## Mapping
//!
//! * A **Highlight** is a row: `text`, `color` (the word: `yellow`, `blue`,
//!   `pink`, `orange`), `chapter` from the current `sectionHeading`, `page`
//!   when the heading gives a number, `pos_kind = kindle_location` and
//!   `pos0`/`pos1` when it gives a location. No date: the export has none. A
//!   highlight with no words is kept `text_unavailable`.
//! * A **Note** is the `origin_note` of the highlight **immediately before it
//!   in the file** (no other heading between them), when that highlight has no
//!   note yet. The app writes a highlight's note straight after it, so
//!   adjacency is the rule; the location is not compared, since the app may
//!   stamp a note at either end of its highlight. Otherwise the note is a
//!   **note-only row** (`text = ""`), keyed by its position, the later of two
//!   at one position winning.
//! * A **Bookmark** is skipped and counted.
//!
//! ## Identity and deletion
//!
//! A highlight's identity is (book title, location as given or else `p` +
//! page, text); a note-only row's is (book title, `note`, position).
//! `IdentityScope::Global`, `origin_id = None`. Deliberately not the clippings'
//! recipe: a Kindle-app highlight and an e-reader clipping of the same passage
//! are two origins and stay two rows (`same_passages` groups them). Two
//! entries with one identity are one row (the first kept).
//!
//! **Nothing is removed.** A re-export missing a highlight is not a deletion:
//! the file is a hand-picked export, not a sync, so its silence about a
//! highlight says nothing.
//!
//! ## Books and copies
//!
//! With a `book_id` the file goes into that book (`NotFound` if it does not
//! exist). Without one, the file's title and authors go through
//! [`crate::koreader`]'s one matcher, **auto-match only**; no match creates no
//! book and returns a report carrying the parsed title, authors and the
//! matcher's candidate band, so a frontend can ask which book. `copy_id` follows
//! [`kindle_copy`]: the book's one Kindle-app (or Kindle-learned) copy, never a
//! new one.
//!
//! Highlight text and notes are the user's private reading: nothing here logs
//! them; log fields carry counts only.
//!
//! [`Storage::upsert_imported_highlight`]: crate::Storage::upsert_imported_highlight
//! [`DiagnosticKind::KindleEntriesUnparsed`]: crate::DiagnosticKind::KindleEntriesUnparsed

use std::collections::HashMap;
use std::path::Path;

use crate::diagnostic::{Diagnostic, ErrorClass};
use crate::error::{EngineError, Result};
use crate::highlight_origin::{
    HighlightImportSummary, HighlightOrigin, ImportedHighlight, PosKind,
};
use crate::kindle_clippings::{
    KindleEntryKind, KindleLocation, KindleWords, LANGUAGES, MAX_TEXT_BYTES, kind_word,
    kindle_copy, page_number, parse_location, read_text_capped, store_highlights, strip_word,
};
use crate::koreader::{MatchCandidate, band, scores_for};
use crate::matching::Query;
use crate::storage::Storage;

/// A notebook export larger than this is not read. A heavily annotated book
/// exports to well under a megabyte.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
/// Headings read from one file; past it they are counted `unparsed`.
pub const MAX_ENTRIES: usize = 100_000;

// ---- scanning -------------------------------------------------------------------

/// The classes the export's body is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    BookTitle,
    Authors,
    Section,
    Heading,
    Text,
}

impl Class {
    fn of(class_attr: &str) -> Option<Class> {
        class_attr.split_whitespace().find_map(|c| {
            Some(match c.to_ascii_lowercase().as_str() {
                "booktitle" => Class::BookTitle,
                "authors" => Class::Authors,
                "sectionheading" => Class::Section,
                "noteheading" => Class::Heading,
                "notetext" => Class::Text,
                _ => return None,
            })
        })
    }
}

/// The end (exclusive) of the tag starting at `lt`, honouring quoted values.
fn tag_end(html: &str, lt: usize) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (i, c) in html[lt + 1..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Some(lt + 1 + i + 1),
            _ => {}
        }
    }
    None
}

/// A tag's name (lowercased) and whether it closes, plus its attribute text.
fn tag_parts(tag: &str) -> (String, bool, &str) {
    let inner = tag.trim_start_matches('<').trim_end_matches('>');
    let (closing, inner) = match inner.strip_prefix('/') {
        Some(r) => (true, r),
        None => (false, inner),
    };
    let name_len = inner
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(inner.len());
    (
        inner[..name_len].to_ascii_lowercase(),
        closing,
        &inner[name_len..],
    )
}

/// The value of attribute `want` in a tag's attribute text: `"…"`, `'…'` or
/// unquoted, names compared case-insensitively.
fn attr(attrs: &str, want: &str) -> Option<String> {
    let mut rest = attrs;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() {
            return None;
        }
        let name_len = rest
            .find(|c: char| c.is_whitespace() || c == '=' || c == '/')
            .unwrap_or(rest.len());
        let name = &rest[..name_len];
        rest = rest[name_len..].trim_start();
        let value = if let Some(r) = rest.strip_prefix('=') {
            let r = r.trim_start();
            let (v, after) = match r.chars().next() {
                Some(q @ ('"' | '\'')) => match r[1..].find(q) {
                    Some(end) => (&r[1..1 + end], &r[1 + end + 1..]),
                    None => (&r[1..], ""),
                },
                _ => {
                    let end = r.find(char::is_whitespace).unwrap_or(r.len());
                    (&r[..end], &r[end..])
                }
            };
            rest = after;
            Some(v)
        } else {
            if name_len == 0 {
                // Not an attribute at all (a stray `=`): skip one character.
                let skip = rest.chars().next().map_or(0, char::len_utf8);
                rest = &rest[skip..];
            }
            None
        };
        if name.eq_ignore_ascii_case(want) {
            return Some(decode(value.unwrap_or("")));
        }
    }
}

/// Every known-class `div` in the file, in order, with its raw content.
fn scan(html: &str) -> Vec<(Class, &str)> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = lower[i..].find('<') {
        let lt = i + off;
        let rest = &lower[lt..];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(html.len(), |e| lt + e + 3);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            i = rest.find('>').map_or(html.len(), |e| lt + e + 1);
            continue;
        }
        let Some(end) = tag_end(html, lt) else {
            break;
        };
        let (name, closing, attrs) = tag_parts(&html[lt..end]);
        i = end;
        if closing {
            continue;
        }
        match name.as_str() {
            "style" | "script" => {
                let close = format!("</{name}");
                i = lower[end..].find(&close).map_or(html.len(), |e| end + e);
            }
            "div" => {
                if let Some(class) = attr(attrs, "class").as_deref().and_then(Class::of) {
                    let stop = content_end(&lower, end);
                    out.push((class, &html[end..stop]));
                    i = stop;
                }
            }
            _ => {}
        }
    }
    out
}

/// Where a known div's content stops: the next `<div`, `</div` or `</h3`.
fn content_end(lower: &str, from: usize) -> usize {
    let mut i = from;
    while let Some(off) = lower[i..].find('<') {
        let at = i + off;
        let rest = &lower[at..];
        for stop in ["<div", "</div", "</h3"] {
            if let Some(after) = rest.strip_prefix(stop)
                && after
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_ascii_alphanumeric())
            {
                return at;
            }
        }
        i = at + 1;
    }
    lower.len()
}

/// The text of a div's content, and the colour of a `highlight_<colour>`
/// span inside it.
fn fragment_text(frag: &str) -> (String, Option<String>) {
    let mut raw = String::new();
    let mut color = None;
    let mut i = 0;
    while i < frag.len() {
        let Some(off) = frag[i..].find('<') else {
            push_text(&frag[i..], &mut raw);
            break;
        };
        let lt = i + off;
        push_text(&frag[i..lt], &mut raw);
        if frag[lt..].starts_with("<!--") {
            i = frag[lt..].find("-->").map_or(frag.len(), |e| lt + e + 3);
            continue;
        }
        let Some(end) = tag_end(frag, lt) else {
            break;
        };
        let (name, closing, attrs) = tag_parts(&frag[lt..end]);
        if !closing {
            match name.as_str() {
                "br" => raw.push('\n'),
                "span" if color.is_none() => {
                    color = attr(attrs, "class").and_then(|c| {
                        c.split_whitespace().find_map(|c| {
                            c.to_ascii_lowercase()
                                .strip_prefix("highlight_")
                                .and_then(color_word)
                        })
                    });
                }
                _ => {}
            }
        }
        i = end;
    }
    let lines: Vec<String> = raw
        .split('\n')
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let first = lines.iter().position(|l| !l.is_empty());
    let last = lines.iter().rposition(|l| !l.is_empty());
    let text = match (first, last) {
        (Some(a), Some(b)) => lines[a..=b].join("\n"),
        _ => String::new(),
    };
    (text, color)
}

/// Raw text: whitespace runs become one space (HTML's rule), then entities.
fn push_text(raw_text: &str, out: &mut String) {
    let mut spaced = String::with_capacity(raw_text.len());
    let mut in_space = false;
    for c in raw_text.chars() {
        if c.is_ascii_whitespace() {
            if !in_space {
                spaced.push(' ');
            }
            in_space = true;
        } else {
            spaced.push(c);
            in_space = false;
        }
    }
    out.push_str(&decode(&spaced));
}

/// Decode character references; an unknown or malformed one is kept as written.
fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let semi = tail
            .char_indices()
            .take(12)
            .find(|(_, c)| *c == ';')
            .map(|(i, _)| i);
        if let Some(c) = semi.and_then(|s| entity(&tail[1..s])) {
            out.push(c);
            rest = &tail[semi.unwrap_or(0) + 1..];
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let n = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(n).filter(|c| *c != '\0');
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "hellip" => '\u{2026}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201C}',
        "rdquo" => '\u{201D}',
        "middot" => '\u{b7}',
        _ => return None,
    })
}

/// A colour word: ASCII letters only, lowercased.
fn color_word(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s.len() <= 24 && s.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| s.to_ascii_lowercase())
}

// ---- headings -------------------------------------------------------------------

/// A page token: a number, a number range, or a roman numeral.
fn page_token(v: &str) -> bool {
    let numeric = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    let roman = |t: &str| !t.is_empty() && t.bytes().all(|b| b"ivxlcdmIVXLCDM".contains(&b));
    match v.split_once('-') {
        Some((a, b)) => numeric(a) && numeric(b),
        None => numeric(v) || roman(v),
    }
}

/// A heading's kind, colour, page and location under one language row, as an
/// entry with no chapter and no text yet. A colour only on a highlight.
fn parse_heading(text: &str, span_color: Option<&str>, w: &KindleWords) -> Option<NotebookEntry> {
    let (kind, rest) = kind_word(text.trim(), w)?;
    let mut rest = rest.trim_start();
    let mut color = span_color.map(str::to_string);
    if let Some(r) = rest.strip_prefix('(') {
        let close = r.find(')')?;
        if color.is_none() {
            color = color_word(&r[..close]);
        }
        rest = r[close + 1..].trim_start();
    }
    let rest = rest
        .strip_prefix(['-', '\u{2013}', '\u{2014}'])
        .unwrap_or(rest);
    let pieces: Vec<&str> = rest
        .split(['\u{b7}', '>', '|'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let (mut page, mut location) = (None, None);
    for p in pieces.iter().rev() {
        if page.is_none()
            && let Some(v) = strip_word(p, w.page).map(str::trim)
            && page_token(v)
        {
            page = Some(v.to_string());
            continue;
        }
        if location.is_none()
            && let Some(l) = strip_word(p, w.location).and_then(|v| parse_location(v.trim()))
        {
            location = Some(l);
            continue;
        }
        break;
    }
    Some(NotebookEntry {
        kind,
        color: color.filter(|_| kind == KindleEntryKind::Highlight),
        chapter: None,
        page,
        location,
        text: String::new(),
    })
}

// ---- parsed shapes --------------------------------------------------------------

/// One heading and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotebookEntry {
    pub kind: KindleEntryKind,
    /// The highlight's colour word, lowercased.
    pub color: Option<String>,
    /// The `sectionHeading` in force.
    pub chapter: Option<String>,
    /// The page as written (`41`, `41-42`, `xii`).
    pub page: Option<String>,
    pub location: Option<KindleLocation>,
    /// Lines joined by `\n`; empty when the heading had no text.
    pub text: String,
}

/// Everything readable in one notebook export.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotebookParse {
    pub title: Option<String>,
    /// `;`-separated in the file; each as written (often `Last, First`).
    pub authors: Vec<String>,
    pub entries: Vec<NotebookEntry>,
    /// Headings seen and not read (no language row, a note with no words,
    /// text too long, past [`MAX_ENTRIES`]) and stray texts with no heading.
    pub unparsed: usize,
    /// The file has a `bookTitle`, `authors` or `noteHeading` div: it looks
    /// like a Kindle notebook export. False means it is not one.
    pub recognised: bool,
}

/// Parse a notebook export. Pure, never panics, no I/O.
pub fn parse_notebook(html: &str) -> NotebookParse {
    enum Expect {
        Nothing,
        Text(NotebookEntry),
        /// The heading was not read; its text goes with it.
        Skip,
    }
    fn settle(e: NotebookEntry, out: &mut NotebookParse) {
        if e.text.len() > MAX_TEXT_BYTES || (e.kind == KindleEntryKind::Note && e.text.is_empty()) {
            out.unparsed += 1;
        } else {
            out.entries.push(e);
        }
    }

    let mut out = NotebookParse::default();
    let mut chapter: Option<String> = None;
    let mut expect = Expect::Nothing;
    for (class, frag) in scan(html) {
        let (text, color) = fragment_text(frag);
        if !matches!(class, Class::Text)
            && let Expect::Text(e) = std::mem::replace(&mut expect, Expect::Nothing)
        {
            settle(e, &mut out);
        }
        match class {
            Class::BookTitle => {
                out.recognised = true;
                if out.title.is_none() && !text.is_empty() {
                    out.title = Some(text.replace('\n', " "));
                }
            }
            Class::Authors => {
                out.recognised = true;
                if out.authors.is_empty() {
                    out.authors = text
                        .replace('\n', " ")
                        .split(';')
                        .map(str::trim)
                        .filter(|a| !a.is_empty())
                        .map(str::to_string)
                        .collect();
                }
            }
            Class::Section => {
                chapter = (!text.is_empty()).then(|| text.replace('\n', " "));
                expect = Expect::Nothing;
            }
            Class::Heading => {
                out.recognised = true;
                if out.entries.len() >= MAX_ENTRIES {
                    out.unparsed += 1;
                    expect = Expect::Skip;
                    continue;
                }
                let line = text.replace('\n', " ");
                expect = match LANGUAGES
                    .iter()
                    .find_map(|w| parse_heading(&line, color.as_deref(), w))
                {
                    Some(e) => Expect::Text(NotebookEntry {
                        chapter: chapter.clone(),
                        ..e
                    }),
                    None => {
                        out.unparsed += 1;
                        Expect::Skip
                    }
                };
            }
            Class::Text => match std::mem::replace(&mut expect, Expect::Nothing) {
                Expect::Text(mut e) => {
                    e.text = text;
                    settle(e, &mut out);
                }
                Expect::Skip => {}
                Expect::Nothing => {
                    if !text.is_empty() {
                        out.unparsed += 1;
                    }
                }
            },
        }
    }
    if let Expect::Text(e) = expect {
        settle(e, &mut out);
    }
    out
}

// ---- becoming highlights --------------------------------------------------------

fn position_key(e: &NotebookEntry) -> String {
    match (&e.location, &e.page) {
        (Some(l), _) => l.as_given(),
        (None, Some(p)) => format!("p{p}"),
        (None, None) => String::new(),
    }
}

fn base_highlight(e: &NotebookEntry, identity: String, text: String) -> ImportedHighlight {
    let mut h = ImportedHighlight::new(HighlightOrigin::KindleApp, identity, text);
    h.chapter = e.chapter.clone();
    h.page = e.page.as_deref().and_then(page_number);
    if let Some(l) = e.location {
        h.pos_kind = Some(PosKind::KindleLocation);
        h.pos0 = Some(l.start.to_string());
        h.pos1 = l.end.map(|e| e.to_string());
    }
    h
}

/// Turn a parse into the highlights the shared path stores, file order: notes
/// paired by adjacency, duplicates collapsed (module doc). `copy_id` is left
/// `None`.
pub fn notebook_highlights(parse: &NotebookParse) -> Vec<ImportedHighlight> {
    let title = parse.title.as_deref().unwrap_or("");
    let mut out: Vec<ImportedHighlight> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut open_highlight: Option<usize> = None;
    for e in &parse.entries {
        match e.kind {
            KindleEntryKind::Highlight => {
                let identity = format!("{title}\u{1f}{}\u{1f}{}", position_key(e), e.text);
                let i = match at.get(&identity) {
                    Some(&i) => i,
                    None => {
                        let mut h = base_highlight(e, identity.clone(), e.text.clone());
                        h.text_unavailable = e.text.is_empty();
                        h.color = e.color.clone();
                        out.push(h);
                        at.insert(identity, out.len() - 1);
                        out.len() - 1
                    }
                };
                open_highlight = out[i].origin_note.is_none().then_some(i);
            }
            KindleEntryKind::Note => {
                if let Some(i) = open_highlight.take() {
                    out[i].origin_note = Some(e.text.clone());
                    continue;
                }
                let key = format!("{title}\u{1f}note\u{1f}{}", position_key(e));
                match at.get(&key) {
                    Some(&i) => out[i].origin_note = Some(e.text.clone()),
                    None => {
                        let mut h = base_highlight(e, key.clone(), String::new());
                        h.origin_note = Some(e.text.clone());
                        out.push(h);
                        at.insert(key, out.len() - 1);
                    }
                }
            }
            KindleEntryKind::Bookmark => open_highlight = None,
        }
    }
    out
}

// ---- importing ------------------------------------------------------------------

/// What an import of one notebook export did (or, dry, would do).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KindleNotebookReport {
    pub dry_run: bool,
    /// The file's own title and authors, so a frontend can ask which book.
    pub title: Option<String>,
    pub authors: Vec<String>,
    /// The book written to. `None`: not matched (or the file unreadable), and
    /// nothing was imported.
    pub book_id: Option<i64>,
    pub copy_id: Option<i64>,
    /// Rows the file holds (highlights and note-only notes), bookmarks
    /// excluded, whether or not they were written.
    pub entries: usize,
    pub bookmarks: usize,
    /// The matcher's ambiguous band when no book matched; else empty.
    pub candidates: Vec<MatchCandidate>,
    /// `removed` is always 0: an export is not a sync.
    pub summary: HighlightImportSummary,
}

/// Import a Kindle app notebook export into `book_id`, or (with `None`) into
/// the book the file's title and authors auto-match. No match creates no book:
/// the report says so and carries what is needed to ask. An unreadable file,
/// or one that is not an export, is `Ok` with a diagnostic; `Err` is a storage
/// failure or a `book_id` that does not exist. `dry_run` writes nothing.
pub async fn import_kindle_notebook(
    storage: &Storage,
    path: &Path,
    book_id: Option<i64>,
    dry_run: bool,
) -> Result<KindleNotebookReport> {
    if let Some(id) = book_id
        && storage.get_book(id).await?.is_none()
    {
        return Err(EngineError::NotFound(format!("book id {id}")));
    }
    let mut report = KindleNotebookReport {
        dry_run,
        ..KindleNotebookReport::default()
    };
    let owned = path.to_path_buf();
    let read = tokio::task::spawn_blocking(move || read_text_capped(&owned, MAX_FILE_BYTES))
        .await
        .map_err(|e| EngineError::Other(format!("notebook read task: {e}")))?;
    let html = match read {
        Ok(t) => t,
        Err((class, detail)) => {
            report
                .summary
                .diagnostics
                .push(Diagnostic::kindle_notebook_unreadable(
                    path.to_path_buf(),
                    class,
                    detail,
                ));
            return Ok(report);
        }
    };
    let parse = parse_notebook(&html);
    import_parsed(storage, path, &parse, book_id, report).await
}

/// The import proper, over an already-parsed file. `path` names the file in
/// diagnostics only.
pub(crate) async fn import_parsed(
    storage: &Storage,
    path: &Path,
    parse: &NotebookParse,
    book_id: Option<i64>,
    mut report: KindleNotebookReport,
) -> Result<KindleNotebookReport> {
    if !parse.recognised {
        report
            .summary
            .diagnostics
            .push(Diagnostic::kindle_notebook_unreadable(
                path.to_path_buf(),
                ErrorClass::Parse,
                "not a Kindle notebook export",
            ));
        return Ok(report);
    }
    report.title = parse.title.clone();
    report.authors = parse.authors.clone();
    report.summary.unparsed = parse.unparsed;
    report.bookmarks = parse
        .entries
        .iter()
        .filter(|e| e.kind == KindleEntryKind::Bookmark)
        .count();
    if parse.unparsed > 0 {
        report
            .summary
            .diagnostics
            .push(Diagnostic::kindle_entries_unparsed(
                path.to_path_buf(),
                parse.unparsed,
            ));
    }
    let highlights = notebook_highlights(parse);
    report.entries = highlights.len();

    let book_id = match book_id {
        Some(id) => id,
        None => {
            let scored =
                scores_for(storage, &Query::new(parse.title.as_deref(), &parse.authors)).await?;
            match scored
                .first()
                .filter(|s| s.can_auto)
                .and_then(|s| s.book.id)
            {
                Some(id) => id,
                None => {
                    report.candidates = band(scored);
                    report
                        .summary
                        .diagnostics
                        .push(Diagnostic::kindle_notebook_unmatched(
                            path.to_path_buf(),
                            parse.title.as_deref(),
                        ));
                    return Ok(report);
                }
            }
        }
    };
    report.book_id = Some(book_id);
    report.copy_id = kindle_copy(storage, book_id).await?;
    let (inserted, updated, unchanged) =
        store_highlights(storage, book_id, report.copy_id, highlights, report.dry_run).await?;
    report.summary.inserted = inserted;
    report.summary.updated = updated;
    report.summary.unchanged = unchanged;
    tracing::debug!(
        inserted,
        updated,
        unchanged,
        unparsed = report.summary.unparsed,
        dry_run = report.dry_run,
        "imported kindle notebook"
    );
    Ok(report)
}

// ---- rendering (tests and fixtures) ---------------------------------------------

/// Write a notebook the way the Kindle app does: an XHTML preamble, a style
/// block, the flat run of divs, a `sectionHeading` whenever the chapter
/// changes, `Highlight(<span …>colour</span>) - Page … · Location …` headings,
/// text escaped and its line breaks as `<br/>`. The inverse of
/// [`parse_notebook`] for well-formed English entries whose chapter, once
/// set, never goes back to none.
#[cfg(any(test, feature = "internals"))]
pub fn render_notebook(title: &str, authors: &[String], entries: &[NotebookEntry]) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
            .replace('\n', "<br/>")
    };
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"\">\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\"><head><meta charset=\"UTF-8\"/>\
         <style>.noteText { font-size: 12pt; } /* <div class=\"noteText\"> */</style><title></title></head>\n<body>\n\
         <div class=\"bodyContainer\">\n<div class=\"notebookFor\">Notebook Export</div>\n",
    );
    out.push_str(&format!(
        "<div class=\"bookTitle\">{}\n</div>\n",
        esc(title)
    ));
    out.push_str(&format!(
        "<div class=\"authors\">\n{}\n</div>\n<div class=\"citation\">\n</div>\n<hr/>\n",
        esc(&authors.join("; "))
    ));
    let mut chapter: Option<&str> = None;
    for e in entries {
        if e.chapter.as_deref() != chapter
            && let Some(c) = e.chapter.as_deref()
        {
            out.push_str(&format!("<div class='sectionHeading'>{}</div>\n", esc(c)));
            chapter = Some(c);
        }
        // The app repeats the chapter path in the heading, before the page.
        let path = e
            .chapter
            .as_deref()
            .map(|c| format!(" {} >", esc(c)))
            .unwrap_or_default();
        let mut heading = match e.kind {
            KindleEntryKind::Highlight => match &e.color {
                Some(c) => format!("Highlight(<span class=\"highlight_{c}\">{c}</span>) -"),
                None => "Highlight -".to_string(),
            },
            KindleEntryKind::Note => "Note -".to_string(),
            KindleEntryKind::Bookmark => "Bookmark -".to_string(),
        };
        let mut pos: Vec<String> = Vec::new();
        if let Some(p) = &e.page {
            pos.push(format!("Page {p}"));
        }
        if let Some(l) = &e.location {
            pos.push(format!("Location {}", l.as_given()));
        }
        if !pos.is_empty() {
            heading.push_str(&path);
            heading.push(' ');
            heading.push_str(&pos.join(" \u{b7} "));
        }
        out.push_str(&format!(
            "<div class=\"noteHeading\">{heading}</div>\n<div class=\"noteText\">{}</div>\n",
            esc(&e.text)
        ));
    }
    out.push_str("</div>\n</body>\n</html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_in_their_variants() {
        let h = |t: &str, span: Option<&str>| {
            parse_heading(t, span, &crate::kindle_clippings::ENGLISH)
                .map(|e| (e.kind, e.color, e.page, e.location))
        };
        let (k, c, p, l) = h(
            "Highlight(yellow) - Chapter 3 > Page 41 \u{b7} Location 612",
            Some("yellow"),
        )
        .unwrap();
        assert_eq!(k, KindleEntryKind::Highlight);
        assert_eq!(c.as_deref(), Some("yellow"));
        assert_eq!(p.as_deref(), Some("41"));
        assert_eq!(
            l,
            Some(KindleLocation {
                start: 612,
                end: None
            })
        );

        let (k, c, p, l) = h("Highlight (blue) - Location 77", None).unwrap();
        assert_eq!(
            (k, c.as_deref(), p, l.map(|l| l.start)),
            (KindleEntryKind::Highlight, Some("blue"), None, Some(77))
        );

        let (k, _, p, l) = h("Note - Page xii", None).unwrap();
        assert_eq!(
            (k, p.as_deref(), l),
            (KindleEntryKind::Note, Some("xii"), None)
        );

        // A chapter called "Page Turner" is not a page.
        let (_, _, p, l) = h("Highlight(pink) - Page Turner > Location 9", None).unwrap();
        assert_eq!((p, l.map(|l| l.start)), (None, Some(9)));

        assert!(h("Surlignement (jaune) - Emplacement 5", None).is_none());
    }

    #[test]
    fn entities_and_breaks() {
        let (t, _) = fragment_text(
            "  Don&#8217;t &amp; won&#x2019;t&nbsp;go<br/>\n  second   line &bogus; &#0; ",
        );
        assert_eq!(
            t,
            "Don\u{2019}t & won\u{2019}t go\nsecond line &bogus; &#0;"
        );
    }

    #[test]
    fn attributes_in_any_order_and_quoting() {
        let html = "<DIV id=x CLASS=noteHeading>Note - Location 3</DIV>\
                    <div data-x='a>b' class = 'noteText other' >hi</h3>\
                    <div class=\"bookTitle\">T</div>";
        let got: Vec<(Class, String)> = scan(html)
            .into_iter()
            .map(|(c, f)| (c, fragment_text(f).0))
            .collect();
        assert_eq!(
            got,
            vec![
                (Class::Heading, "Note - Location 3".into()),
                (Class::Text, "hi".into()),
                (Class::BookTitle, "T".into()),
            ]
        );
    }

    #[test]
    fn a_note_pairs_only_with_the_highlight_right_before_it() {
        let e = |kind, loc: u64, text: &str| NotebookEntry {
            kind,
            color: None,
            chapter: None,
            page: None,
            location: Some(KindleLocation {
                start: loc,
                end: None,
            }),
            text: text.into(),
        };
        use KindleEntryKind::*;
        let parse = NotebookParse {
            title: Some("T".into()),
            entries: vec![
                e(Highlight, 10, "one"),
                e(Note, 10, "on one"),
                e(Note, 11, "second note, alone"),
                e(Highlight, 20, "two"),
                e(Bookmark, 21, ""),
                e(Note, 20, "after a bookmark, alone"),
            ],
            recognised: true,
            ..Default::default()
        };
        let got: Vec<(String, Option<String>)> = notebook_highlights(&parse)
            .into_iter()
            .map(|h| (h.text, h.origin_note))
            .collect();
        assert_eq!(
            got,
            vec![
                ("one".into(), Some("on one".into())),
                (String::new(), Some("second note, alone".into())),
                ("two".into(), None),
                (String::new(), Some("after a bookmark, alone".into())),
            ]
        );
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::book::Book;
    use proptest::prelude::*;

    fn words() -> impl Strategy<Value = String> {
        proptest::collection::vec("[A-Za-z0-9&<>\"'’,.;é]{1,9}", 1..8).prop_map(|w| w.join(" "))
    }

    fn text(lines: std::ops::Range<usize>) -> impl Strategy<Value = String> {
        proptest::collection::vec(words(), lines).prop_map(|l| l.join("\n"))
    }

    fn entry() -> impl Strategy<Value = NotebookEntry> {
        (
            prop_oneof![
                3 => Just(KindleEntryKind::Highlight),
                2 => Just(KindleEntryKind::Note),
                1 => Just(KindleEntryKind::Bookmark)
            ],
            proptest::option::of(prop_oneof![
                Just("yellow".to_string()),
                Just("blue".to_string()),
                Just("pink".to_string()),
                Just("orange".to_string())
            ]),
            // Never `Page …`/`Location …`: a chapter so named is read as one,
            // which the module doc states.
            proptest::option::of("Ch[a-z]{2,8}( [0-9]{1,2})?"),
            proptest::option::of(prop_oneof!["[1-9][0-9]{0,3}", "[ivx]{1,4}"]),
            proptest::option::of(1u64..50_000),
            text(1..4),
            text(0..3),
        )
            .prop_map(
                |(kind, color, chapter, page, loc, some, any)| NotebookEntry {
                    kind,
                    color: (kind == KindleEntryKind::Highlight)
                        .then_some(color)
                        .flatten(),
                    chapter,
                    page,
                    location: loc.map(|start| KindleLocation { start, end: None }),
                    text: match kind {
                        KindleEntryKind::Note => some,
                        KindleEntryKind::Highlight => any,
                        KindleEntryKind::Bookmark => String::new(),
                    },
                },
            )
    }

    /// Once a chapter is set it stays set (the export cannot say "no chapter").
    fn entries() -> impl Strategy<Value = Vec<NotebookEntry>> {
        proptest::collection::vec(entry(), 0..20).prop_map(|mut v| {
            let mut last: Option<String> = None;
            for e in &mut v {
                if e.chapter.is_none() {
                    e.chapter = last.clone();
                }
                last = e.chapter.clone();
            }
            v
        })
    }

    fn hostile() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                Just("<div class=\"noteHeading\">".to_string()),
                Just("<div class='noteText'>".to_string()),
                Just("<div class=bookTitle>".to_string()),
                Just("</div>".to_string()),
                Just("<br/>".to_string()),
                Just("<span class=\"highlight_".to_string()),
                Just("Highlight(".to_string()),
                Just(" - Page ".to_string()),
                Just(" \u{b7} Location ".to_string()),
                Just("&#".to_string()),
                Just("&amp".to_string()),
                Just("<!--".to_string()),
                Just("<style>".to_string()),
                Just("\"".to_string()),
                Just("'".to_string()),
                Just("<".to_string()),
                "[0-9]{1,25}",
                "\\PC{0,12}",
            ],
            0..80,
        )
        .prop_map(|v| v.concat())
    }

    proptest! {
        #[test]
        fn parsing_never_panics(s in "\\PC{0,400}", h in hostile()) {
            let _ = notebook_highlights(&parse_notebook(&s));
            let p = parse_notebook(&h);
            prop_assert!(p.entries.len() <= MAX_ENTRIES);
            let _ = notebook_highlights(&p);
        }

        #[test]
        fn render_then_parse_is_the_identity(
            title in words(),
            authors in proptest::collection::vec("[A-Z][a-z]{1,8}, [A-Z][a-z]{1,8}", 0..3),
            entries in entries(),
        ) {
            let p = parse_notebook(&render_notebook(&title, &authors, &entries));
            prop_assert!(p.recognised);
            prop_assert_eq!(p.unparsed, 0);
            prop_assert_eq!(p.title.as_deref(), Some(title.as_str()));
            prop_assert_eq!(p.authors, authors);
            prop_assert_eq!(p.entries, entries);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        #[test]
        fn a_second_import_changes_nothing(entries in entries()) {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                let s = Storage::connect("sqlite::memory:").await.unwrap();
                let b = Book {
                    title: Some("The Remains of the Day".into()),
                    authors: vec!["Ishiguro, Kazuo".into()],
                    ..Default::default()
                };
                s.upsert_book(&b, None).await.unwrap();
                let html = render_notebook("The Remains of the Day", &["Ishiguro, Kazuo".into()], &entries);
                let parse = parse_notebook(&html);
                let path = Path::new("notebook.html");
                let fresh = || KindleNotebookReport::default();
                let first = import_parsed(&s, path, &parse, None, fresh()).await.unwrap();
                prop_assert!(first.book_id.is_some());
                prop_assert_eq!(first.summary.inserted, first.entries);
                let again = import_parsed(&s, path, &parse, None, fresh()).await.unwrap();
                prop_assert_eq!(again.summary.inserted, 0);
                prop_assert_eq!(again.summary.updated, 0);
                prop_assert_eq!(again.summary.unchanged, first.entries);
                Ok(())
            })?;
        }
    }
}
