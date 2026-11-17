use std::path::{Path, PathBuf};

use regex::Regex;
use time::OffsetDateTime;
use time::macros::format_description;

use crate::book::Book;
use crate::error::{EngineError, Result};
use crate::storage::Storage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoteKind {
    #[default]
    Note,
    Session,
    Reflection,
    /// Public prose, and the only kind that carries a rating; never derived from a reflection.
    Review,
}

impl NoteKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NoteKind::Note => "note",
            NoteKind::Session => "session",
            NoteKind::Reflection => "reflection",
            NoteKind::Review => "review",
        }
    }

    pub fn is_anchored(&self) -> bool {
        matches!(self, NoteKind::Reflection | NoteKind::Review)
    }
}

impl std::str::FromStr for NoteKind {
    type Err = EngineError;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "note" => Ok(NoteKind::Note),
            "session" => Ok(NoteKind::Session),
            // `final` is the old name for a reflection; parsed, never written.
            "reflection" | "final" => Ok(NoteKind::Reflection),
            "review" => Ok(NoteKind::Review),
            other => Err(EngineError::InvalidInput(format!(
                "unknown note kind: {other}"
            ))),
        }
    }
}

#[derive(Debug, Default)]
pub struct NewNoteInput {
    pub book_id: Option<i64>,
    /// Always set for a reflection or a review; `None` for an ordinary note.
    pub reading_id: Option<i64>,
    pub highlight_id: Option<i64>,
    pub page: Option<i64>,
    pub location: Option<String>,
    pub kind: NoteKind,
    pub title: Option<String>,
    pub body: String,
}

#[derive(Debug)]
pub struct CreatedNote {
    pub id: i64,
    pub title: String,
    pub file: PathBuf,
    pub links: Vec<String>,
}

pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = true;
    for c in s.chars() {
        // The lowercase expansion is filtered: `'İ'` lowercases to `i` plus a combining mark, which would
        // otherwise survive the first pass and become a dash on the second, breaking idempotence.
        let lowered = if c.is_alphanumeric() {
            c.to_lowercase()
                .filter(|l| l.is_alphanumeric())
                .collect::<String>()
        } else {
            String::new()
        };
        if !lowered.is_empty() {
            out.push_str(&lowered);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= 60 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

fn wikilink_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    // Group 2 (the alias) is for `excerpt` only; a capture group does not change what matches.
    RE.get_or_init(|| {
        Regex::new(r"\[\[([^\]\|#]+)(?:#[^\]\|]*)?(?:\|([^\]]*))?\]\]").expect("static regex")
    })
}

/// Every wikilink in `body` as (byte range of the `[[…]]` token, trimmed target); not deduped.
pub fn wikilink_spans(body: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    wikilink_re()
        .captures_iter(body)
        .filter_map(|cap| {
            let whole = cap.get(0)?;
            let target = cap.get(1)?.as_str().trim();
            (!target.is_empty()).then_some((whole.range(), target))
        })
        .collect()
}

pub fn extract_wikilinks(body: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for (_, target) in wikilink_spans(body) {
        let target = target.to_string();
        if !seen.contains(&target) {
            seen.push(target);
        }
    }
    seen
}

/// Byte cap on a link paragraph; past it the paragraph is cut on a char boundary around the link.
pub const LINK_PARAGRAPH_CAP: usize = 600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkParagraph {
    pub text: String,
    pub link_range: (usize, usize),
    pub truncated: bool,
    pub truncated_start: bool,
}

pub fn link_paragraph(body: &str, target_title: &str) -> Option<LinkParagraph> {
    let spans = wikilink_spans(body);
    let (range, _) = spans
        .iter()
        .find(|(_, t)| *t == target_title)
        .or_else(|| {
            spans
                .iter()
                .find(|(_, t)| t.eq_ignore_ascii_case(target_title))
        })?
        .clone();

    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    for (i, b) in body.bytes().enumerate() {
        if b == b'\n' {
            lines.push((start, i));
            start = i + 1;
        }
    }
    lines.push((start, body.len()));
    let trim_cr = |(s, e): (usize, usize)| {
        if e > s && body.as_bytes()[e - 1] == b'\r' {
            (s, e - 1)
        } else {
            (s, e)
        }
    };
    let blank = |&(s, e): &(usize, usize)| body[s..e].trim().is_empty();

    // A link cannot usefully span a blank line, so its end is clamped into the paragraph.
    let at = lines
        .iter()
        .position(|&(s, e)| range.start >= s && range.start <= e)?;
    let mut first = at;
    while first > 0 && !blank(&lines[first - 1]) {
        first -= 1;
    }
    let mut last = at;
    while last + 1 < lines.len() && !blank(&lines[last + 1]) {
        last += 1;
    }
    let p_start = lines[first].0;
    let p_end = trim_cr(lines[last]).1.max(range.end.min(body.len()));
    let link = (range.start - p_start, range.end - p_start);
    let para = &body[p_start..p_end];

    if para.len() <= LINK_PARAGRAPH_CAP {
        return Some(LinkParagraph {
            text: para.to_string(),
            link_range: link,
            truncated: false,
            truncated_start: false,
        });
    }
    let link_len = link.1 - link.0;
    let (w_start, w_end) = if link.1 <= LINK_PARAGRAPH_CAP {
        (0, floor_char(para, LINK_PARAGRAPH_CAP))
    } else if link_len >= LINK_PARAGRAPH_CAP {
        link
    } else {
        let slack = (LINK_PARAGRAPH_CAP - link_len) / 2;
        let s = floor_char(para, link.0 - slack);
        let e = floor_char(para, (s + LINK_PARAGRAPH_CAP).min(para.len()));
        (s, e.max(link.1))
    };
    Some(LinkParagraph {
        text: para[w_start..w_end].to_string(),
        link_range: (link.0 - w_start, link.1 - w_start),
        truncated: true,
        truncated_start: w_start > 0,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone)]
pub struct LinkContext {
    pub direction: LinkDirection,
    pub target_title: String,
    /// The other note; `None` only for an outgoing forward reference.
    pub note: Option<crate::storage::NoteRecord>,
    pub book: Option<Book>,
    /// Incoming only: where the link sits in the other note's body; `None` otherwise or if unreadable.
    pub paragraph: Option<LinkParagraph>,
}

fn floor_char(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn frontmatter(
    book: Option<&Book>,
    reading_id: Option<i64>,
    highlight_id: Option<i64>,
    page: Option<i64>,
    location: Option<&str>,
    kind: NoteKind,
    created: &str,
) -> String {
    let mut fm = String::from("---\n");
    if let Some(b) = book {
        let id = b
            .any_isbn()
            .map(str::to_string)
            .or(b.id.map(|i| i.to_string()))
            .unwrap_or_default();
        fm.push_str(&format!("book: {id}\n"));
        fm.push_str(&format!(
            "book-title: \"{}\"\n",
            b.display_title().replace('"', "'")
        ));
    }
    if let Some(r) = reading_id {
        fm.push_str(&format!("reading: {r}\n"));
    }
    if let Some(h) = highlight_id {
        fm.push_str(&format!("highlight: {h}\n"));
    }
    if let Some(p) = page {
        fm.push_str(&format!("page: {p}\n"));
    }
    if let Some(l) = location {
        fm.push_str(&format!("location: \"{}\"\n", l.replace('"', "'")));
    }
    fm.push_str(&format!("kind: {}\n", kind.as_str()));
    fm.push_str(&format!("created: {created}\n"));
    fm.push_str("---\n\n");
    fm
}

fn is_line_end(after: &str) -> bool {
    after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n")
}

fn find_closing_fence(rest: &str) -> Option<usize> {
    // The fence can be the first line (`---\n---\n`), which a search for `\n---` misses.
    if let Some(after) = rest.strip_prefix("---")
        && is_line_end(after)
    {
        return Some("---".len());
    }
    let mut from = 0;
    while let Some(rel) = rest[from..].find("\n---") {
        let past = from + rel + "\n---".len();
        if is_line_end(&rest[past..]) {
            return Some(past);
        }
        from += rel + 1;
    }
    None
}

/// Splits a note file into its raw frontmatter header (through the closing `---` and any blank
/// line) and body; with no frontmatter the header is empty.
pub fn frontmatter_and_body(content: &str) -> (&str, &str) {
    let Some(rest) = content.strip_prefix("---\n") else {
        return ("", content);
    };
    let Some(past_fence) = find_closing_fence(rest) else {
        return ("", content);
    };
    let after_close = "---\n".len() + past_fence;
    let body_start = after_close + content[after_close..].len()
        - content[after_close..].trim_start_matches('\n').len();
    (&content[..body_start], &content[body_start..])
}

pub fn parse_frontmatter(content: &str) -> (Vec<(String, String)>, &str) {
    let (header, body) = frontmatter_and_body(content);
    if header.is_empty() {
        return (Vec::new(), body);
    }
    let pairs = header
        .lines()
        .filter(|l| *l != "---")
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        })
        .collect();
    (pairs, body)
}

fn derive_title(body: &str) -> String {
    let words: Vec<&str> = body.split_whitespace().take(6).collect();
    if words.is_empty() {
        "Untitled".to_string()
    } else {
        words
            .join(" ")
            .trim_end_matches([',', '.', ';', ':'])
            .to_string()
    }
}

/// The body's first non-blank line when it is `# Something` (one `#`); `## Sub` is not a name.
pub fn heading_of(body: &str) -> Option<&str> {
    let line = body.lines().find(|l| !l.trim().is_empty())?;
    let heading = line.trim_end().strip_prefix("# ")?.trim();
    (!heading.is_empty()).then_some(heading)
}

pub fn title_for(typed: Option<&str>, body: &str) -> String {
    typed
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .or_else(|| heading_of(body).map(str::to_string))
        .unwrap_or_else(|| derive_title(body))
}

/// Whether `title` is what `derive_title` gives part 1 of `body`. Part 1, not the whole body, so a
/// short first part does not borrow words from the first marker.
pub fn title_is_derived(title: &str, body: &str) -> bool {
    let first = split_parts(body)
        .first()
        .map(|p| p.text)
        .unwrap_or_default();
    title == derive_title(first)
}

/// `Some` when the body opens with a `# heading` or the title was typed; always `None` for a
/// reflection or review.
pub fn row_heading(kind: &str, title: &str, body: &str) -> Option<String> {
    if matches!(kind, "reflection" | "review") {
        return None;
    }
    if let Some(h) = heading_of(body) {
        return Some(h.to_string());
    }
    (!title_is_derived(title, body)).then(|| title.to_string())
}

/// Byte cap on a row's excerpt, cut on a char boundary with no ellipsis (a frontend adds one).
pub const EXCERPT_CAP: usize = 280;

fn md_link_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"!?\[([^\]\n]*)\]\([^)\n]*\)").expect("static regex"))
}

fn line_marker_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*(?:#{1,6}[ \t]+|>[ \t]?|[-*+][ \t]+|\d+[.)][ \t]+)+")
            .expect("static regex")
    })
}

pub fn excerpt(text: &str) -> String {
    let plain = plain_text(text);
    let cut = floor_char(&plain, EXCERPT_CAP);
    plain[..cut].to_string()
}

fn plain_text(text: &str) -> String {
    let mut body = text.trim_start_matches(['\n', '\r']);
    let first = body.lines().next().unwrap_or("");
    if heading_of(first).is_some() {
        body = body.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
    }

    let linked = wikilink_re().replace_all(body, |cap: &regex::Captures| {
        let alias = cap.get(2).map(|m| m.as_str().trim()).unwrap_or("");
        if alias.is_empty() {
            cap.get(1)
                .map(|m| m.as_str().trim())
                .unwrap_or("")
                .to_string()
        } else {
            alias.to_string()
        }
    });
    let linked = md_link_re().replace_all(&linked, "$1");
    let unstructured = line_marker_re().replace_all(&linked, "");

    let chars: Vec<char> = unstructured.chars().collect();
    let mut plain = String::with_capacity(unstructured.len());
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '*' | '`' => continue,
            '~' if chars.get(i + 1) == Some(&'~') || (i > 0 && chars[i - 1] == '~') => continue,
            '_' => {
                let before = i > 0 && chars[i - 1].is_alphanumeric();
                let after = chars.get(i + 1).is_some_and(|c| c.is_alphanumeric());
                if before && after {
                    plain.push(c); // snake_case is a word, not emphasis
                }
            }
            _ => plain.push(c),
        }
    }

    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The line beginning part 2 onward: `<!-- part 2 · 2026-11-04T21:10:00Z · p. 251 -->`.
/// Recognised only as a whole line of exactly this grammar; position, not `n`, numbers a part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartMarker {
    pub n: i64,
    pub at: i64,
    pub page: Option<i64>,
}

fn marker_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^<!-- part (\d{1,9}) · (\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})Z(?: · p\. (\d{1,9}))? -->$",
        )
        .expect("static regex")
    })
}

const MARKER_STAMP: &[time::format_description::FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");

impl PartMarker {
    pub fn line(&self) -> String {
        let at = OffsetDateTime::from_unix_timestamp(self.at)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
            .format(MARKER_STAMP)
            .unwrap_or_default();
        match self.page {
            Some(p) => format!("<!-- part {} · {at}Z · p. {p} -->", self.n),
            None => format!("<!-- part {} · {at}Z -->", self.n),
        }
    }

    pub fn parse(line: &str) -> Option<PartMarker> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let cap = marker_re().captures(line)?;
        let n = cap[1].parse().ok()?;
        let at = time::PrimitiveDateTime::parse(&cap[2], MARKER_STAMP)
            .ok()?
            .assume_utc()
            .unix_timestamp();
        let page = match cap.get(3) {
            Some(p) => Some(p.as_str().parse().ok()?),
            None => None,
        };
        Some(PartMarker { n, at, page })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyPart<'a> {
    /// `None` for part 1, which has no marker.
    pub marker: Option<PartMarker>,
    /// Leading blank lines and trailing whitespace removed; never contains a marker line.
    pub text: &'a str,
    /// Bytes between this part's marker line and the next; what `replace_part` rewrites.
    pub segment: std::ops::Range<usize>,
}

fn normalise_segment(seg: &str) -> &str {
    let mut s = seg;
    while let Some(i) = s.find('\n') {
        if s[..i].trim().is_empty() {
            s = &s[i + 1..];
        } else {
            break;
        }
    }
    let s = s.trim_end();
    if s.trim().is_empty() { "" } else { s }
}

fn line_ranges(body: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, b) in body.bytes().enumerate() {
        if b == b'\n' {
            out.push((start, i));
            start = i + 1;
        }
    }
    out.push((start, body.len()));
    out
}

/// Every part in file order; always at least one.
pub fn split_parts(body: &str) -> Vec<BodyPart<'_>> {
    let mut parts = Vec::new();
    let mut marker: Option<PartMarker> = None;
    let mut seg_start = 0;
    for (ls, le) in line_ranges(body) {
        if let Some(m) = PartMarker::parse(&body[ls..le]) {
            parts.push(BodyPart {
                marker,
                text: normalise_segment(&body[seg_start..ls]),
                segment: seg_start..ls,
            });
            marker = Some(m);
            seg_start = le;
        }
    }
    parts.push(BodyPart {
        marker,
        text: normalise_segment(&body[seg_start..]),
        segment: seg_start..body.len(),
    });
    parts
}

/// True when some line is a part marker: such text cannot be a part's.
pub fn contains_marker(text: &str) -> bool {
    text.lines().any(|l| PartMarker::parse(l).is_some())
}

/// A body from its parts; the inverse of `split_parts` for already-normal part texts.
pub fn render_parts(first: &str, rest: &[(PartMarker, &str)]) -> String {
    let mut out = first.to_string();
    for (m, text) in rest {
        out.push_str("\n\n");
        out.push_str(&m.line());
        out.push_str("\n\n");
        out.push_str(text);
    }
    out
}

pub fn append_part(body: &str, marker: PartMarker, text: &str) -> String {
    format!(
        "{}\n\n{}\n\n{}",
        body.trim_end(),
        marker.line(),
        normalise_segment(text)
    )
}

/// `body` with part `index` (0-based) rewritten, markers byte-identical; `None` for a missing part.
pub fn replace_part(body: &str, index: usize, text: &str) -> Option<String> {
    let parts = split_parts(body);
    let part = parts.get(index)?;
    let text = normalise_segment(text);
    let more = index + 1 < parts.len();
    let segment = match (index, more) {
        (0, true) => format!("{text}\n\n"),
        (0, false) => text.to_string(),
        (_, true) => format!("\n\n{text}\n\n"),
        (_, false) => format!("\n\n{text}"),
    };
    Some(format!(
        "{}{segment}{}",
        &body[..part.segment.start],
        &body[part.segment.end..]
    ))
}

/// Part count and newest marker date; `None` for a body with no marker.
pub fn part_stamps(body: &str) -> (i64, Option<i64>) {
    let parts = split_parts(body);
    let latest = parts.iter().filter_map(|p| p.marker.map(|m| m.at)).max();
    (parts.len() as i64, latest)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotePart {
    /// 1-based position in the file, as `UpdateNotePart` addresses it.
    pub n: i64,
    /// Unix seconds; part 1's is the note's `created_at`.
    pub at: i64,
    /// Part 1's is the note's `page`.
    pub page: Option<i64>,
    pub body: String,
}

pub fn note_parts(body: &str, created_at: i64, page: Option<i64>) -> Vec<NotePart> {
    split_parts(body)
        .into_iter()
        .enumerate()
        .map(|(i, p)| NotePart {
            n: i as i64 + 1,
            at: p.marker.map_or(created_at, |m| m.at),
            page: p.marker.map_or(page, |m| m.page),
            body: p.text.to_string(),
        })
        .collect()
}

/// The part a row speaks for: newest by date, the later in the file on a tie.
pub fn latest_part(parts: &[NotePart]) -> Option<&NotePart> {
    parts.iter().max_by_key(|p| (p.at, p.n))
}

fn chars_eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

fn match_ci(hay: &str, needle: &str) -> Option<usize> {
    let mut h = hay.char_indices();
    for nc in needle.chars() {
        let (_, hc) = h.next()?;
        if !chars_eq_ci(hc, nc) {
            return None;
        }
    }
    Some(h.next().map_or(hay.len(), |(i, _)| i))
}

/// Whether `title` can be a `[[wikilink]]` target: `]`, `|`, `#` are reserved; `[` opens a link.
pub fn linkable_title(title: &str) -> bool {
    let t = title.trim();
    !t.is_empty() && !t.contains(['[', ']', '|', '#'])
}

/// The first case-insensitive whole-phrase mention of `phrase` outside a `[[…]]` and a marker line.
/// Shared by offering and linking mentions, so an offer is always one the link can honour.
pub fn find_mention(body: &str, phrase: &str) -> Option<std::ops::Range<usize>> {
    let phrase = phrase.trim();
    if phrase.is_empty() {
        return None;
    }
    let mut fenced: Vec<std::ops::Range<usize>> =
        wikilink_spans(body).into_iter().map(|(r, _)| r).collect();
    for (ls, le) in line_ranges(body) {
        if PartMarker::parse(&body[ls..le]).is_some() {
            fenced.push(ls..le);
        }
    }
    for (i, _) in body.char_indices() {
        let Some(len) = match_ci(&body[i..], phrase) else {
            continue;
        };
        let r = i..i + len;
        let before = body[..i].chars().next_back();
        let after = body[r.end..].chars().next();
        if before.is_some_and(char::is_alphanumeric) || after.is_some_and(char::is_alphanumeric) {
            continue;
        }
        if fenced.iter().any(|f| f.start < r.end && r.start < f.end) {
            continue;
        }
        // The link must read back as itself: an unclosed `[[` earlier in the line would swallow it.
        let wrapped = format!("{}[[{phrase}]]{}", &body[..r.start], &body[r.end..]);
        let reads_back = wikilink_spans(&wrapped)
            .iter()
            .any(|(span, target)| span.start == r.start && *target == phrase);
        if !reads_back {
            continue;
        }
        return Some(r);
    }
    None
}

/// `body` with its first unlinked mention of `title` wrapped: `[[as written]]`, or
/// `[[Title|as written]]` when only the case differs.
pub fn link_first_mention(body: &str, title: &str) -> Option<String> {
    let title = title.trim();
    let r = find_mention(body, title)?;
    let written = &body[r.clone()];
    let link = if written == title {
        format!("[[{written}]]")
    } else {
        format!("[[{title}|{written}]]")
    };
    Some(format!("{}{link}{}", &body[..r.start], &body[r.end..]))
}

pub async fn create_note(
    storage: &Storage,
    vault_dir: &Path,
    book: Option<&Book>,
    input: NewNoteInput,
) -> Result<CreatedNote> {
    let title = title_for(input.title.as_deref(), &input.body);

    let book_dir = book
        .map(|b| slugify(b.display_title()))
        .unwrap_or_else(|| "unsorted".to_string());
    let dir = vault_dir.join(&book_dir);
    std::fs::create_dir_all(&dir)?;

    let now = OffsetDateTime::now_utc();
    let stamp = now.format(format_description!(
        "[year][month][day][hour][minute][second]"
    ))?;
    let base = format!("{stamp}-{}", slugify(&title));
    let mut file_name = format!("{base}.md");
    let mut n = 1;
    while dir.join(&file_name).exists() {
        n += 1;
        file_name = format!("{base}-{n}.md");
    }
    let file = dir.join(&file_name);
    let rel_path = format!("{book_dir}/{file_name}");

    let created_str = now.format(&time::format_description::well_known::Rfc3339)?;
    let content = format!(
        "{}{}\n",
        frontmatter(
            book,
            input.reading_id,
            input.highlight_id,
            input.page,
            input.location.as_deref(),
            input.kind,
            &created_str,
        ),
        input.body.trim_end()
    );
    std::fs::write(&file, &content)?;

    // Index the body as written, not as passed in: they differ by a trim, and a mismatch would
    // make the whole vault re-index.
    let (_, written_body) = frontmatter_and_body(&content);
    let links = extract_wikilinks(written_body);
    let inserted = storage
        .insert_note(
            crate::storage::NewNoteMeta {
                book_id: input.book_id,
                reading_id: input.reading_id,
                highlight_id: input.highlight_id,
                page: input.page,
                location: input.location.as_deref(),
                file_path: &rel_path,
                title: &title,
                kind: input.kind.as_str(),
            },
            written_body,
            &links,
        )
        .await;
    let id = match inserted {
        Ok(id) => id,
        Err(e) => {
            // No row claims the file, so nothing would ever adopt it.
            std::fs::remove_file(&file).ok();
            return Err(e);
        }
    };

    Ok(CreatedNote {
        id,
        title,
        file,
        links,
    })
}

/// What a vault sweep found. `absent` exists so a missing file is asserted never to delete a note;
/// a frontend must not present it as work owed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VaultReconcile {
    pub checked: usize,
    pub reindexed: usize,
    pub absent: usize,
}

/// A note file's body, or `None` when absent. Any other read error propagates.
pub(crate) fn body_on_disk(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let (_, body) = frontmatter_and_body(&content);
            Ok(Some(body.to_string()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Brings one note's index in line with a body read from its file: the single path for the
/// watcher, the sweep and refresh. Compared trim-end, so an editor's trailing newline is no change.
pub(crate) async fn reindex_from_body(
    storage: &Storage,
    note_id: i64,
    title: &str,
    body: &str,
) -> Result<bool> {
    let indexed = storage.indexed_body(note_id).await?;
    if indexed.as_deref().map(str::trim_end) == Some(body.trim_end()) {
        return Ok(false);
    }
    let links = extract_wikilinks(body);
    storage.reindex_note(note_id, title, body, &links).await?;
    Ok(true)
}

/// A path under the vault as `notes.file_path` spells it, joined with `/` on every platform;
/// `None` for anything that is not a markdown file inside the vault.
pub(crate) fn vault_relative(vault: &Path, path: &Path) -> Option<String> {
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
    {
        return None;
    }
    let rest = path.strip_prefix(vault).ok()?;
    let parts: Vec<&str> = rest
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?;
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Walks every note and reindexes those whose file is ahead: the cold half a watcher cannot do.
/// `stat` first, then compare; `last_modified` is in seconds, so a same-second edit is missed.
pub(crate) async fn reconcile_vault(storage: &Storage, vault: &Path) -> Result<VaultReconcile> {
    let mut report = VaultReconcile::default();
    for note in storage.note_files().await? {
        let path = vault.join(&note.file_path);
        report.checked += 1;

        let Ok(meta) = std::fs::metadata(&path) else {
            // Absent, unreadable or a directory is not a deletion.
            report.absent += 1;
            continue;
        };
        let newer = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .is_none_or(|d| d.as_secs() as i64 > note.last_modified);
        if !newer {
            continue;
        }
        let Some(body) = body_on_disk(&path)? else {
            report.absent += 1;
            continue;
        };
        if reindex_from_body(storage, note.id, &note.title, &body).await? {
            report.reindexed += 1;
        }
    }
    // The path stays out of the log: a filename is the user's prose, not above `trace!`.
    tracing::debug!(
        checked = report.checked,
        reindexed = report.reindexed,
        absent = report.absent,
        "vault reconciled"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoteScope;

    #[test]
    fn slugs_are_obsidian_safe() {
        assert_eq!(
            slugify("Dorothea's Choice: part 1"),
            "dorothea-s-choice-part-1"
        );
        assert_eq!(slugify("a/b\\c#d^e[f]g|h"), "a-b-c-d-e-f-g-h");
        assert_eq!(slugify("  ---  "), "untitled");
        assert!(slugify(&"x".repeat(200)).len() <= 60);
    }

    #[test]
    fn wikilink_extraction_handles_aliases_and_headings() {
        let body = "See [[Vocation]] and [[Zettelkasten|the method]], plus [[Middlemarch#Chapter 2]].\
                    Repeat [[Vocation]]. Not a link: [single].";
        assert_eq!(
            extract_wikilinks(body),
            vec![
                "Vocation".to_string(),
                "Zettelkasten".to_string(),
                "Middlemarch".to_string()
            ]
        );
    }

    #[test]
    fn link_paragraph_is_the_run_of_lines_around_the_link() {
        let body = "First para.\n\nSecond line one\nsees [[Vocation|it]] here.\n  \nThird.";
        let p = link_paragraph(body, "Vocation").unwrap();
        assert_eq!(p.text, "Second line one\nsees [[Vocation|it]] here.");
        assert_eq!(&p.text[p.link_range.0..p.link_range.1], "[[Vocation|it]]");
        assert!(!p.truncated && !p.truncated_start);
        assert_eq!(link_paragraph(body, "vocation"), Some(p));
        assert_eq!(link_paragraph(body, "Middlemarch"), None);
    }

    #[test]
    fn link_paragraph_takes_the_first_occurrence_and_drops_crlf() {
        let body = "a [[X]]\r\n\r\nb [[X]]\r\n";
        let p = link_paragraph(body, "X").unwrap();
        assert_eq!(p.text, "a [[X]]");
        assert_eq!(p.link_range, (2, 7));
    }

    #[test]
    fn a_long_paragraph_is_cut_around_the_link() {
        let tail = "é".repeat(400);
        let body = format!("[[X]] {tail}");
        let p = link_paragraph(&body, "X").unwrap();
        assert!(p.truncated && !p.truncated_start);
        assert!(p.text.len() <= LINK_PARAGRAPH_CAP);
        assert_eq!(p.link_range, (0, 5));

        let head = "ü".repeat(500);
        let body = format!("{head} [[X]] {tail}");
        let p = link_paragraph(&body, "X").unwrap();
        assert!(p.truncated && p.truncated_start);
        assert!(p.text.len() <= LINK_PARAGRAPH_CAP);
        assert_eq!(&p.text[p.link_range.0..p.link_range.1], "[[X]]");
    }

    #[test]
    fn frontmatter_roundtrip() {
        let book = Book {
            id: Some(7),
            title: Some("Middlemarch".into()),
            isbn_13: Some("9781900000178".into()),
            ..Default::default()
        };
        let fm = frontmatter(
            Some(&book),
            Some(11),
            Some(3),
            Some(42),
            Some("Chapter 2"),
            NoteKind::Session,
            "2026-07-23T10:00:00Z",
        );
        let content = format!("{fm}The body text with [[Link]].\n");
        let (pairs, body) = parse_frontmatter(&content);
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("book"), Some("9781900000178"));
        assert_eq!(get("book-title"), Some("Middlemarch"));
        assert_eq!(get("reading"), Some("11"));
        assert_eq!(get("highlight"), Some("3"));
        assert_eq!(get("page"), Some("42"));
        assert_eq!(get("location"), Some("Chapter 2"));
        assert_eq!(get("kind"), Some("session"));
        assert_eq!(get("created"), Some("2026-07-23T10:00:00Z"));
        assert_eq!(body, "The body text with [[Link]].\n");
    }

    #[test]
    fn frontmatter_and_body_splits_and_preserves_header() {
        let content = "---\npage: 5\nkind: note\n---\n\nThe body\nspans lines.\n";
        let (header, body) = frontmatter_and_body(content);
        assert_eq!(header, "---\npage: 5\nkind: note\n---\n\n");
        assert_eq!(body, "The body\nspans lines.\n");
        assert_eq!(
            format!("{header}rewritten\n"),
            "---\npage: 5\nkind: note\n---\n\nrewritten\n"
        );

        let (h2, b2) = frontmatter_and_body("just text\n");
        assert_eq!(h2, "");
        assert_eq!(b2, "just text\n");
    }

    #[test]
    fn a_horizontal_rule_in_the_body_is_not_mistaken_for_the_closing_fence() {
        let content = "---\nkind: note\n---\n\nIntro paragraph.\n\n----\n\nAfter the rule.\n";
        let (header, body) = frontmatter_and_body(content);

        assert_eq!(header, "---\nkind: note\n---\n\n");
        assert_eq!(body, "Intro paragraph.\n\n----\n\nAfter the rule.\n");

        let dashed = "---\nkind: note\n---\n\nSee ---foo below.\n";
        let (h, b) = frontmatter_and_body(dashed);
        assert_eq!(h, "---\nkind: note\n---\n\n");
        assert_eq!(b, "See ---foo below.\n");
    }

    #[test]
    fn an_empty_frontmatter_block_is_still_frontmatter() {
        let (h, b) = frontmatter_and_body("---\n---\n\nbody text\n");
        assert_eq!(h, "---\n---\n\n");
        assert_eq!(b, "body text\n");

        let (h2, b2) = frontmatter_and_body("---\n---\n");
        assert_eq!(h2, "---\n---\n");
        assert_eq!(b2, "");
    }

    #[test]
    fn the_split_always_partitions_the_input() {
        for content in [
            "",
            "---",
            "---\n",
            "---\n---\n",
            "---\nkind: note\n---\n\nbody\n",
            "no frontmatter at all\n",
            "---\nkind: note\n\nunterminated frontmatter\n",
            "---\na: b\n---\n\n----\n\nrule\n",
            "---\r\na: b\r\n---\r\n\r\nCRLF body\r\n",
            "text\n---\nnot frontmatter, wrong position\n",
        ] {
            let (h, b) = frontmatter_and_body(content);
            assert_eq!(
                format!("{h}{b}"),
                content,
                "split lost or duplicated bytes for {content:?}"
            );
        }
    }

    #[test]
    fn a_heading_is_one_hash_and_a_space_on_the_first_line() {
        assert_eq!(
            heading_of("\n\n# maps that lie  \nbody"),
            Some("maps that lie")
        );
        assert_eq!(heading_of("## sub\nbody"), None);
        assert_eq!(heading_of("#tag body"), None);
        assert_eq!(heading_of("text\n# later"), None);
        assert_eq!(heading_of("#   "), None);
        assert_eq!(title_for(None, "# maps that lie\nx"), "maps that lie");
        assert_eq!(
            title_for(Some("  "), "# h\nx"),
            "h",
            "a blank title is no title"
        );
        assert_eq!(title_for(Some("Typed"), "# h\nx"), "Typed");
        assert_eq!(
            title_for(None, "six words make a derived title here"),
            "six words make a derived title"
        );
    }

    #[test]
    fn an_excerpt_is_plain_short_and_drops_the_heading() {
        let body = "# Title\n\n> Quoted **bold** and _em_ in snake_case, `code`, ~~gone~~.\n\
                    - [[Vocation|calling]] then [[Middlemarch#Ch 2]] and [a link](http://x) ![alt](i.png)\n\
                    1. last";
        assert_eq!(
            excerpt(body),
            "Quoted bold and em in snake_case, code, gone. calling then Middlemarch and a link alt last"
        );
        let long = "é".repeat(400);
        let cut = excerpt(&long);
        assert!(cut.len() <= EXCERPT_CAP && long.starts_with(&cut));
        assert_eq!(excerpt(""), "");
    }

    #[test]
    fn a_marker_is_exactly_its_grammar() {
        let m = PartMarker {
            n: 2,
            at: 1_793_826_600,
            page: Some(251),
        };
        assert_eq!(m.line(), "<!-- part 2 · 2026-11-04T21:10:00Z · p. 251 -->");
        assert_eq!(PartMarker::parse(&m.line()), Some(m));
        assert_eq!(PartMarker::parse(&format!("{}\r", m.line())), Some(m));
        let bare = PartMarker { page: None, ..m };
        assert_eq!(
            PartMarker::parse("<!-- part 2 · 2026-11-04T21:10:00Z -->"),
            Some(bare)
        );
        for not in [
            "<!-- part 2 · 2026-11-04T21:10:00Z -->x",
            " <!-- part 2 · 2026-11-04T21:10:00Z -->",
            "<!-- part 2 - 2026-11-04T21:10:00Z -->",
            "<!-- part 2 · 2026-13-04T21:10:00Z -->",
            "<!-- an ordinary comment -->",
        ] {
            assert_eq!(PartMarker::parse(not), None, "{not:?}");
        }
    }

    #[test]
    fn replacing_a_part_leaves_every_marker_byte_identical() {
        let m2 = PartMarker {
            n: 2,
            at: 100,
            page: Some(3),
        };
        let m3 = PartMarker {
            n: 7,
            at: 200,
            page: None,
        };
        let body = render_parts("one", &[(m2, "two"), (m3, "three")]);
        let edited = replace_part(&body, 1, "\n\nTWO\n\n").unwrap();
        assert!(edited.contains(&m2.line()) && edited.contains(&m3.line()));
        let texts: Vec<_> = split_parts(&edited).iter().map(|p| p.text).collect();
        assert_eq!(texts, vec!["one", "TWO", "three"]);
        let first = replace_part(&body, 0, "ONE").unwrap();
        assert_eq!(split_parts(&first)[0].text, "ONE");
        let last = replace_part(&body, 2, "").unwrap();
        assert_eq!(
            split_parts(&last).len(),
            3,
            "an emptied part is still a part"
        );
        assert_eq!(replace_part(&body, 3, "x"), None);
        assert_eq!(part_stamps(&body), (3, Some(200)));
        assert_eq!(part_stamps("plain"), (1, None));
    }

    #[test]
    fn a_mention_is_whole_unlinked_and_case_blind() {
        let body = "Roadmaps that lie. [[X|Maps that lie]]. MAPS THAT LIE!";
        let r = find_mention(body, "maps that lie").unwrap();
        assert_eq!(&body[r], "MAPS THAT LIE");
        assert_eq!(
            link_first_mention(body, "Maps that lie").unwrap(),
            "Roadmaps that lie. [[X|Maps that lie]]. [[Maps that lie|MAPS THAT LIE]]!"
        );
        assert_eq!(
            link_first_mention("see Vocation here", "Vocation").unwrap(),
            "see [[Vocation]] here"
        );
        assert_eq!(find_mention("Vocational", "Vocation"), None);
        assert!(!linkable_title("a|b") && !linkable_title("") && linkable_title("Vocation"));
    }

    #[test]
    fn derive_title_takes_leading_words() {
        assert_eq!(
            derive_title("Dorothea's choice mirrors the whole town, somehow."),
            "Dorothea's choice mirrors the whole town"
        );
        assert_eq!(derive_title(""), "Untitled");
    }

    #[tokio::test]
    async fn create_note_writes_file_and_indexes() {
        let scratch = tempfile::tempdir().unwrap();
        let vault = scratch.path().join("vault");

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let book_id = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let book = s.get_book(book_id).await.unwrap().unwrap();

        let created = create_note(
            &s,
            &vault,
            Some(&book),
            NewNoteInput {
                book_id: Some(book_id),
                body: "Dorothea's dignity under [[Vocation]] pressure.".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert!(created.file.exists());
        let content = std::fs::read_to_string(&created.file).unwrap();
        assert!(content.starts_with("---\n"));
        assert!(content.contains("kind: note"));
        assert_eq!(created.links, vec!["Vocation".to_string()]);

        let hits = s
            .search_marks("dignity", Some(crate::SearchSource::Note), None, 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].as_note().unwrap().id, created.id);
    }

    fn markdown_under(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(markdown_under(&path));
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
        out
    }

    #[tokio::test]
    async fn a_note_whose_row_is_refused_leaves_no_file() {
        let scratch = tempfile::tempdir().unwrap();
        let vault = scratch.path().join("vault");
        let s = Storage::connect("sqlite::memory:").await.unwrap();

        // A book deleted after the caller looked it up: the row's foreign key is refused.
        let input = || NewNoteInput {
            book_id: Some(4242),
            body: "Lydgate's ambitions.".into(),
            ..Default::default()
        };
        for _ in 0..2 {
            assert!(create_note(&s, &vault, None, input()).await.is_err());
        }

        assert_eq!(markdown_under(&vault), Vec::<PathBuf>::new());
        assert!(s.note_files().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_retry_after_a_refused_insert_makes_exactly_one_note() {
        let scratch = tempfile::tempdir().unwrap();
        let vault = scratch.path().join("vault");
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TRIGGER refuse_notes BEFORE INSERT ON notes
             BEGIN SELECT RAISE(ABORT, 'refused'); END",
        )
        .execute(s.pool())
        .await
        .unwrap();

        let input = || NewNoteInput {
            body: "Casaubon's key to all mythologies.".into(),
            ..Default::default()
        };
        assert!(create_note(&s, &vault, None, input()).await.is_err());

        sqlx::query("DROP TRIGGER refuse_notes")
            .execute(s.pool())
            .await
            .unwrap();
        let created = create_note(&s, &vault, None, input()).await.unwrap();

        assert_eq!(markdown_under(&vault), vec![created.file.clone()]);
        let rows = s.note_files().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(vault.join(&rows[0].file_path), created.file);
    }

    #[tokio::test]
    async fn anchor_lands_in_frontmatter_and_db() {
        let scratch = tempfile::tempdir().unwrap();
        let vault = scratch.path().join("vault");

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let book_id = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let book = s.get_book(book_id).await.unwrap().unwrap();

        let created = create_note(
            &s,
            &vault,
            Some(&book),
            NewNoteInput {
                book_id: Some(book_id),
                page: Some(128),
                location: Some("Chapter 3".into()),
                body: "The register shifts here.".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(&created.file).unwrap();
        assert!(content.contains("page: 128"));
        assert!(content.contains("location: \"Chapter 3\""));

        let rec = s.list_notes(NoteScope::Book(book_id), None).await.unwrap();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].page, Some(128));
        assert_eq!(rec[0].location.as_deref(), Some("Chapter 3"));
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn the_split_is_always_a_partition(content in ".{0,200}") {
            let (h, b) = frontmatter_and_body(&content);
            prop_assert_eq!(format!("{h}{b}"), content.clone());
        }

        #[test]
        fn note_shaped_input_splits_cleanly(
            keys in proptest::collection::vec("[a-z-]{1,10}", 0..5),
            body in ".{0,80}",
        ) {
            let mut content = String::from("---\n");
            for (i, k) in keys.iter().enumerate() {
                content.push_str(&format!("{k}: value{i}\n"));
            }
            content.push_str("---\n\n");
            content.push_str(&body);

            let (h, b) = frontmatter_and_body(&content);
            prop_assert_eq!(format!("{h}{b}"), content.clone());
            prop_assert!(h.starts_with("---\n"), "header lost its opening fence");
            prop_assert_eq!(b, body.as_str());
        }

        #[test]
        fn a_slug_is_always_a_safe_filename(s in ".{0,120}") {
            let slug = slugify(&s);
            prop_assert!(!slug.is_empty());
            prop_assert!(!slug.starts_with('-') && !slug.ends_with('-'), "{:?}", slug);
            prop_assert!(!slug.contains("--"), "{:?}", slug);
            prop_assert!(slug != "." && slug != "..", "{:?}", slug);
            prop_assert!(
                !slug.contains('/') && !slug.contains('\\') && !slug.contains(':'),
                "path separator in {:?}", slug
            );
            // The 60 cap is checked after pushing, so multibyte input can overshoot by one char's width.
            prop_assert!(slug.len() <= 60 + 4, "unbounded slug: {} bytes", slug.len());
            prop_assert_eq!(slugify(&slug), slug.clone(), "slugify is not idempotent");
        }

        #[test]
        fn wikilink_targets_are_always_substrings_of_the_body(body in ".{0,200}") {
            let links = extract_wikilinks(&body);
            let mut seen = Vec::new();
            for l in &links {
                prop_assert!(!l.is_empty());
                prop_assert!(body.contains(l.as_str()), "{:?} not in body", l);
                prop_assert!(!l.contains(']') && !l.contains('|') && !l.contains('#'), "{:?}", l);
                prop_assert!(!seen.contains(l), "duplicate target {:?}", l);
                seen.push(l.clone());
            }
        }

        #[test]
        fn a_link_paragraph_always_holds_its_link(
            before in "[a-zé \n]{0,700}",
            after in "[a-zé \n]{0,700}",
            name in "[a-z]{1,8}",
        ) {
            let body = format!("{before}[[{name}]]{after}");
            let p = link_paragraph(&body, &name).expect("the link is in the body");
            let (s, e) = p.link_range;
            prop_assert!(p.text.is_char_boundary(s) && p.text.is_char_boundary(e));
            prop_assert_eq!(&p.text[s..e], format!("[[{}]]", name));
            prop_assert!(body.contains(p.text.as_str()));
            prop_assert!(p.text.len() <= LINK_PARAGRAPH_CAP + 4, "{} bytes", p.text.len());
            prop_assert!(!p.truncated_start || p.truncated);
        }

        #[test]
        fn an_excerpt_is_a_bounded_prefix(body in "(?s).{0,700}") {
            let e = excerpt(&body);
            let plain = plain_text(&body);
            prop_assert!(e.len() <= EXCERPT_CAP, "{} bytes", e.len());
            prop_assert!(plain.is_char_boundary(e.len()));
            prop_assert_eq!(&plain[..e.len()], e.as_str());
            prop_assert!(e.len() == plain.len() || plain.len() > EXCERPT_CAP);
        }

        #[test]
        fn parts_round_trip(
            first in "[a-z #*]{0,30}(\n[a-z #*]{1,30}){0,3}",
            rest in proptest::collection::vec(
                (0i64..4_102_444_800, proptest::option::of(0i64..100_000),
                 "[a-z #*]{0,30}(\n\n?[a-z #*]{1,30}){0,3}"),
                0..5,
            ),
        ) {
            let norm = |t: &str| normalise_segment(t).to_string();
            let first = norm(&first);
            let rest: Vec<(PartMarker, String)> = rest
                .iter()
                .enumerate()
                .map(|(i, (at, page, t))| (PartMarker { n: i as i64 + 2, at: *at, page: *page }, norm(t)))
                .collect();
            let borrowed: Vec<(PartMarker, &str)> = rest.iter().map(|(m, t)| (*m, t.as_str())).collect();
            let body = render_parts(&first, &borrowed);
            let parsed = split_parts(&body);
            prop_assert_eq!(parsed.len(), rest.len() + 1);
            prop_assert_eq!(parsed[0].marker, None);
            prop_assert_eq!(parsed[0].text, first.as_str());
            for (p, (m, t)) in parsed[1..].iter().zip(&rest) {
                prop_assert_eq!(p.marker, Some(*m));
                prop_assert_eq!(p.text, t.as_str());
            }
            prop_assert_eq!(part_stamps(&body).0, parsed.len() as i64);
        }

        #[test]
        fn continuing_never_touches_an_earlier_part(
            body in "(?s)[a-z <!>\n-]{0,200}",
            text in "[a-z]{1,20}",
            at in 0i64..4_102_444_800,
        ) {
            let before: Vec<String> = split_parts(&body).iter().map(|p| p.text.to_string()).collect();
            let after = append_part(&body, PartMarker { n: 99, at, page: None }, &text);
            let parts = split_parts(&after);
            prop_assert_eq!(parts.len(), before.len() + 1);
            for (p, t) in parts.iter().zip(&before) {
                prop_assert_eq!(p.text, t.as_str());
            }
            prop_assert_eq!(parts.last().unwrap().text, text.as_str());
        }

        #[test]
        fn a_found_mention_links_cleanly(
            before in "[a-zé \\[\\]|]{0,60}",
            after in "[a-zé \\[\\]|]{0,60}",
            phrase in "[a-z]{3,8}( [a-z]{2,5})?",
        ) {
            let body = format!("{before} {} {after}", phrase.to_uppercase());
            if let Some(r) = find_mention(&body, &phrase) {
                prop_assert!(body.is_char_boundary(r.start) && body.is_char_boundary(r.end));
                prop_assert_eq!(body[r].to_lowercase(), phrase.clone());
                let linked = link_first_mention(&body, &phrase).unwrap();
                prop_assert!(extract_wikilinks(&linked).contains(&phrase));
            }
        }

        #[test]
        fn well_formed_wikilinks_are_extracted(
            names in proptest::collection::vec("[a-z][a-z ]{0,10}", 1..4),
        ) {
            let body: String = names
                .iter()
                .map(|n| format!("[[{n}]] "))
                .collect();
            let got = extract_wikilinks(&body);
            let mut want: Vec<String> = Vec::new();
            for n in &names {
                let t = n.trim().to_string();
                if !t.is_empty() && !want.contains(&t) {
                    want.push(t);
                }
            }
            prop_assert_eq!(got, want);
        }
    }
}
