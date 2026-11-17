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
    /// Small reading-session thought.
    Session,
    /// Private, the hub of the graph: cites highlights, links notes and other
    /// reflections, and is **openable mid-book** rather than written at the end.
    /// Supersedes the old `final` (migration `0007` rewrites those rows).
    Reflection,
    /// Public: prose for other people, and the only kind that carries a rating.
    /// Never derived from the reflection — a review is a rewrite for a different
    /// audience, not a subset of private thinking.
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

    /// True for the two kinds that anchor to a reading and are opened through
    /// [`crate::Engine::open_reflection`] / [`crate::Engine::open_review`]
    /// rather than written like an ordinary note.
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
            // `final` is what a reflection used to be called, and old vault
            // files still say it in their frontmatter. Parsed, never written.
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
    /// The reading this note belongs to. Always set for a reflection or a
    /// review; `None` for an ordinary note, which floats free of any one
    /// reading.
    pub reading_id: Option<i64>,
    pub highlight_id: Option<i64>,
    /// Device/pdf page the note anchors to (auto-suggested from the book's
    /// current progress by the frontend, editable per note).
    pub page: Option<i64>,
    /// Free-form location: chapter name, "loc 1234", a percentage, etc.
    pub location: Option<String>,
    pub kind: NoteKind,
    pub title: Option<String>,
    pub body: String,
}

#[derive(Debug)]
pub struct CreatedNote {
    pub id: i64,
    pub title: String,
    /// Absolute path of the markdown file on disk.
    pub file: PathBuf,
    pub links: Vec<String>,
}

/// Obsidian-safe kebab slug: strips `: / \ # ^ [ ] |` and friends.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = true;
    for c in s.chars() {
        // **The lowercase expansion is filtered, not trusted.** `char::to_lowercase`
        // can return more than one char, and the extra one need not be
        // alphanumeric: `'İ'` (U+0130, the Turkish dotted capital I) lowercases to
        // `i` + U+0307 COMBINING DOT ABOVE, and a combining mark is `Mn` rather
        // than alphanumeric. Pushing it whole made `slugify` non-idempotent — the
        // mark survived the first pass and became a dash on the second, so the
        // same title produced two different filenames depending on how many times
        // it had been through here. Found by `a_slug_is_always_a_safe_filename`,
        // which is the property that exists to find exactly this.
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

/// The one wikilink grammar: `[[target]]`, `[[target|alias]]`,
/// `[[target#heading]]`. Shared by [`extract_wikilinks`] (which writes the
/// edges) and [`wikilink_spans`] (which finds where an edge sits in a body), so
/// a link the index holds is always a link the paragraph finder can see.
fn wikilink_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    // Group 2 (the alias) is captured for [`excerpt`] only. A capture group
    // changes what is *reported*, never what *matches*, so the edge set the
    // index holds is untouched by it.
    RE.get_or_init(|| {
        Regex::new(r"\[\[([^\]\|#]+)(?:#[^\]\|]*)?(?:\|([^\]]*))?\]\]").expect("static regex")
    })
}

/// Every wikilink in `body`, in body order, with the byte range of the whole
/// `[[…]]` token and its trimmed target. Not deduped — a body that links one
/// note twice has two spans. Empty targets (`[[ ]]`) are skipped, as
/// [`extract_wikilinks`] skips them.
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

/// Extract [[wikilink]] targets, handling `[[target|alias]]` and
/// `[[target#heading]]`; deduped, order preserved.
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

/// The most of a paragraph [`link_paragraph`] returns, in bytes. A paragraph
/// is a row's sentence, not the note: past this it is cut on a char boundary
/// around the link and flagged.
pub const LINK_PARAGRAPH_CAP: usize = 600;

/// The paragraph of a note body that holds a link, with the link's place in
/// it. Derived here rather than in a frontend because the link grammar is the
/// engine's (`wikilink_spans`), and a second parser would find a different
/// set of links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkParagraph {
    /// The paragraph text — possibly cut to [`LINK_PARAGRAPH_CAP`] bytes.
    pub text: String,
    /// Byte range of the whole `[[…]]` token inside `text`. Always on char
    /// boundaries and always inside `text`.
    pub link_range: (usize, usize),
    /// Some of the paragraph was cut, at either end.
    pub truncated: bool,
    /// The cut included the paragraph's head, so the text begins mid-paragraph
    /// (an ellipsis belongs before it as well as, or instead of, after it).
    pub truncated_start: bool,
}

/// Find the first link to `target_title` in `body` and the paragraph it sits
/// in.
///
/// **The paragraph rule:** the run of lines between blank lines (a line that
/// is empty or whitespace only) around the link, without its trailing line
/// break. A paragraph longer than [`LINK_PARAGRAPH_CAP`] bytes is cut to a
/// window of at most that many bytes on char boundaries: from the paragraph's
/// start when the link ends within the cap (only the tail is lost), otherwise
/// centred on the link. A link token longer than the cap is returned whole.
///
/// The target is compared as `note_links.target_title` stores it — trimmed,
/// exact first and then ASCII-case-insensitively, which is SQLite's `NOCASE`
/// and so how `write_links` resolved the edge. `None` when the body no longer
/// holds the link (an index a save has not caught up with).
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

    // Lines as byte ranges, newline excluded (a trailing `\r` too).
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

    // The line holding the link's first byte; a link cannot span a blank line
    // usefully, so its end is clamped into the paragraph below.
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

/// Which end of an edge a [`LinkContext`] is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkDirection {
    /// The other note links to this one.
    Incoming,
    /// This note links to the other.
    Outgoing,
}

/// One link of a note, read with what a row needs to say what it is: the
/// other note, the book that note is against, and — for a link **in** — the
/// paragraph of the other note that holds it (`Engine::link_contexts`).
#[derive(Debug, Clone)]
pub struct LinkContext {
    pub direction: LinkDirection,
    /// The `[[wikilink]]` target as the linking body wrote it.
    pub target_title: String,
    /// The other note. `None` only for an outgoing forward reference to a
    /// note not written yet.
    pub note: Option<crate::storage::NoteRecord>,
    /// The other note's book, when it is against one.
    pub book: Option<Book>,
    /// Incoming only: where the link sits in the other note's body. `None`
    /// for an outgoing link (its sentence is in this note's own body), and
    /// for an incoming one whose body is unreadable or no longer holds it.
    pub paragraph: Option<LinkParagraph>,
}

/// The largest char boundary of `s` at or below `i`.
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
    // Beside the book, because it is part of the same anchor: which book, and
    // which time through it.
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

/// True when a closing `---` is the whole line — i.e. what follows the three
/// dashes is a line break or the end of input. This is what separates a real
/// fence from a `----` horizontal rule or a `---foo`.
fn is_line_end(after: &str) -> bool {
    after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n")
}

/// Byte offset within `rest` just past a closing `---` fence, or `None`.
fn find_closing_fence(rest: &str) -> Option<usize> {
    // The fence can be the very first line — an empty frontmatter block,
    // `---\n---\n`. Searching only for `"\n---"` misses it, because there is no
    // preceding newline to match, and the whole file then reads as body.
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

/// Split a note file into its raw frontmatter header (through the closing
/// `---` and any blank line after it) and the body. When there is no
/// frontmatter the header is empty and the body is the whole content.
pub fn frontmatter_and_body(content: &str) -> (&str, &str) {
    let Some(rest) = content.strip_prefix("---\n") else {
        return ("", content);
    };
    // The closing fence is a line that is exactly `---`. Searching for a bare
    // `"\n---"` also matched `\n----` (an ordinary markdown horizontal rule)
    // and `\n---foo`, either of which would be taken as the terminator and
    // mangle the split — losing part of the user's note body into the header.
    let Some(past_fence) = find_closing_fence(rest) else {
        return ("", content);
    };
    // Byte offset just past the closing `---`, then past its trailing newlines.
    let after_close = "---\n".len() + past_fence;
    let body_start = after_close + content[after_close..].len()
        - content[after_close..].trim_start_matches('\n').len();
    (&content[..body_start], &content[body_start..])
}

/// Split a note file into (frontmatter key/value pairs, body).
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

// ---- headings --------------------------------------------------------------

/// The heading a body opens with: its first non-blank line, when that line is
/// `# Something` — exactly one `#` and a space, CommonMark's level-one ATX
/// heading. `None` for anything else, including `## Sub` (a subheading is not
/// what a note is *called*) and a bare `#`.
///
/// This is the whole of the heading rule, and it lives beside [`derive_title`]
/// because the two are the two halves of one question: where a note's name
/// comes from when nobody typed one into a title box.
pub fn heading_of(body: &str) -> Option<&str> {
    let line = body.lines().find(|l| !l.trim().is_empty())?;
    let heading = line.trim_end().strip_prefix("# ")?.trim();
    (!heading.is_empty()).then_some(heading)
}

/// The title a new note takes: the one typed, else the body's `# heading`,
/// else the first six words. A blank typed title is no title — a title box
/// left empty is not a request to call the note `""`, which no `[[wikilink]]`
/// can name.
pub fn title_for(typed: Option<&str>, body: &str) -> String {
    typed
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .or_else(|| heading_of(body).map(str::to_string))
        .unwrap_or_else(|| derive_title(body))
}

/// Is this title the one [`derive_title`] would give part 1 of this body?
///
/// Measured against **part 1** rather than the whole body, because a parted
/// note's raw body continues past part 1 and a short first part would
/// otherwise borrow words from the first marker. One honest limit, stated: the
/// derived title is frozen at creation (re-deriving it would repoint every
/// backlink), so a reader who later rewrites the first six words makes the old
/// title read as typed. It then shows as a heading — which is also what it now
/// is, a name the body no longer spells.
pub fn title_is_derived(title: &str, body: &str) -> bool {
    let first = split_parts(body)
        .first()
        .map(|p| p.text)
        .unwrap_or_default();
    title == derive_title(first)
}

/// What a list row draws as a heading: `Some` when the body opens with a
/// `# heading` (that line's text) or when the stored title is not the derived
/// first six words (a title the reader typed); `None` otherwise, and always
/// `None` for a reflection or a review, whose titles are generated
/// (*Reflection: X*) and say nothing the kind word beside them does not.
pub fn row_heading(kind: &str, title: &str, body: &str) -> Option<String> {
    if matches!(kind, "reflection" | "review") {
        return None;
    }
    if let Some(h) = heading_of(body) {
        return Some(h.to_string());
    }
    (!title_is_derived(title, body)).then(|| title.to_string())
}

// ---- excerpts --------------------------------------------------------------

/// The most of a note's own words a list row carries, in bytes. Two lines of a
/// row at the app's measure, with room; the cut is on a char boundary and no
/// ellipsis is added — whether to draw one is a frontend's choice, since it
/// is the one that knows whether the text overflowed its box.
pub const EXCERPT_CAP: usize = 280;

fn md_link_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"!?\[([^\]\n]*)\]\([^)\n]*\)").expect("static regex"))
}

fn line_marker_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    // Headings, blockquotes, bullets and ordered-list numbers at a line start —
    // the structure a row has no room to show.
    RE.get_or_init(|| {
        Regex::new(r"(?m)^[ \t]*(?:#{1,6}[ \t]+|>[ \t]?|[-*+][ \t]+|\d+[.)][ \t]+)+")
            .expect("static regex")
    })
}

/// A part's text as a list row reads it: plain, short, one line.
///
/// The rule, entire: leading blank lines go and a `# heading` first line is
/// dropped (the row draws the heading separately); `[[target|alias]]`
/// becomes `alias`, `[[target#h]]` and `[[target]]` become `target`;
/// `[text](url)` and `![alt](url)` become their text; line-start structure
/// (headings, `>`, bullets, `1.`) goes; emphasis marks (`*`, `~~`, `` ` ``, and
/// `_` where it is not inside a word) go; every run of whitespace becomes one
/// space; and the result is cut to [`EXCERPT_CAP`] bytes on a char boundary.
///
/// Here rather than in a frontend because the wikilink grammar is the
/// engine's ([`wikilink_spans`]): a second parser would read a different set
/// of links out of the same body.
pub fn excerpt(text: &str) -> String {
    let plain = plain_text(text);
    let cut = floor_char(&plain, EXCERPT_CAP);
    plain[..cut].to_string()
}

/// [`excerpt`] before the cut — the whole part as one plain line.
fn plain_text(text: &str) -> String {
    // Drop leading blank lines, then a heading first line.
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

// ---- parts -----------------------------------------------------------------

/// The marker line that begins part 2 onward of a continued note:
///
/// ```text
/// <!-- part 2 · 2026-11-04T21:10:00Z · p. 251 -->
/// ```
///
/// `· p. N` only when a page was given. A marker is recognised **only as a
/// whole line matching exactly that grammar** (a trailing `\r` aside); any
/// other HTML comment is body text. The timestamp is UTC to the second, the
/// rest of the engine's convention.
///
/// `n` is what the line says, and it is **not** what a part is numbered by:
/// [`split_parts`] numbers by position, because an outside edit can delete a
/// marker and the honest count is the one the file now holds. The engine
/// writes `n` equal to the position, so in a file nobody else touched the two
/// agree.
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
    /// The line, without its line break.
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

    /// The marker this line is, or `None` when it is body text.
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

/// One part of a body, as [`split_parts`] finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyPart<'a> {
    /// `None` for part 1, which has no marker.
    pub marker: Option<PartMarker>,
    /// The part's text: its segment with leading blank lines and trailing
    /// whitespace removed. Never contains a marker line.
    pub text: &'a str,
    /// The byte range between this part's marker line (exclusive of the line
    /// itself) and the next marker line — what [`replace_part`] rewrites, so the
    /// markers stay byte-identical.
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

/// Lines of `body` as `(start, end_of_content)` byte offsets, newline excluded.
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

/// Every part of a body, in file order. Always at least one: a body with no
/// marker is one part, and an empty body is one empty part.
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

/// True when some line of `text` is a part marker — the text cannot be a
/// part's, because the file would read it as two.
pub fn contains_marker(text: &str) -> bool {
    text.lines().any(|l| PartMarker::parse(l).is_some())
}

/// A body from its parts: part 1's text, then `\n\n<marker>\n\n<text>` for each
/// part after it. The inverse of [`split_parts`] for parts whose texts are
/// already normal (no leading blank line, no trailing whitespace, no marker
/// line) — the property `parse(render(parts)) == parts` is over exactly those.
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

/// `body` with one more part on the end. The earlier parts' texts are
/// untouched by construction: the body is only trimmed at its end, which
/// normalisation already ignores, and the new text sits after a fresh marker.
pub fn append_part(body: &str, marker: PartMarker, text: &str) -> String {
    format!(
        "{}\n\n{}\n\n{}",
        body.trim_end(),
        marker.line(),
        normalise_segment(text)
    )
}

/// `body` with part `index` (0-based) rewritten to `text`, every marker line
/// left byte-identical. `None` when there is no such part.
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

/// How many parts a body holds and the newest marker date in it — the two
/// facts migration `0023` indexes. `None` for a body with no marker, whose one
/// part is dated by the note's own `created_at`.
pub fn part_stamps(body: &str) -> (i64, Option<i64>) {
    let parts = split_parts(body);
    let latest = parts.iter().filter_map(|p| p.marker.map(|m| m.at)).max();
    (parts.len() as i64, latest)
}

/// One part of a note, dated and paged — what `NoteParts` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotePart {
    /// 1-based position in the file — the number `UpdateNotePart` addresses.
    pub n: i64,
    /// Unix seconds. Part 1's is the note's `created_at`.
    pub at: i64,
    /// Part 1's is the note's `page`.
    pub page: Option<i64>,
    pub body: String,
}

/// A note's parts from its body and the two facts part 1 borrows from the row.
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

/// The part a list row speaks for: the newest by date, the later in the file
/// on a tie (an outside edit can leave two markers with one stamp). Never
/// `None` — [`note_parts`] always yields at least one.
pub fn latest_part(parts: &[NotePart]) -> Option<&NotePart> {
    parts.iter().max_by_key(|p| (p.at, p.n))
}

// ---- mentions --------------------------------------------------------------

fn chars_eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// The byte length of `needle` at the head of `hay`, compared char by char
/// without regard to case; `None` when it is not there.
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

/// Can `title` be written as a `[[wikilink]]` target at all? The grammar
/// reserves `]`, `|` and `#`, and a `[` would open a second link.
pub fn linkable_title(title: &str) -> bool {
    let t = title.trim();
    !t.is_empty() && !t.contains(['[', ']', '|', '#'])
}

/// The first place `body` mentions `phrase` without linking it: a
/// case-insensitive whole-phrase occurrence (no letter or digit touching
/// either end) that is not inside a `[[…]]` and not on a part marker line.
///
/// Shared by `UnlinkedMentions` (which only offers what this finds) and
/// `LinkMention` (which wraps what this finds), so an offer is always one the
/// link can honour.
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
        // The link must read back as itself: an unclosed `[[` earlier in the
        // line would swallow a wrapped mention into a different target. Found
        // by `a_found_mention_links_cleanly`.
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

/// `body` with its first unlinked mention of `title` wrapped as a link:
/// `[[as written]]` when the text matches the title exactly, and
/// `[[Title|as written]]` when only the case differs, so the sentence reads as
/// it did and the edge resolves to the note by its own name.
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

/// Write the markdown file into the vault and index it (metadata, FTS,
/// wikilink edges) in the database.
pub async fn create_note(
    storage: &Storage,
    vault_dir: &Path,
    book: Option<&Book>,
    input: NewNoteInput,
) -> Result<CreatedNote> {
    // A typed title, else the body's `# heading`, else the first six words.
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

    // **Index the body that was actually written**, not the one that was
    // passed in. They differ by a trim and a newline, and once that
    // difference meant a note was never byte-identical to its own index — so
    // the first time anything compared the file against the index it found a
    // change that had not happened, and re-indexed the entire vault.
    let (_, written_body) = frontmatter_and_body(&content);
    let links = extract_wikilinks(written_body);
    let id = storage
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
        .await?;

    Ok(CreatedNote {
        id,
        title,
        file,
        links,
    })
}

// ---- vault coherence --------------------------------------------------------

/// What a sweep of the vault found, in the past tense.
///
/// **Not a to-do list.** `absent` counts notes whose file was not there when we
/// looked; it exists so the non-destructive rule (a missing file is never a
/// deletion) can be *asserted*, and
/// so the engine can say something in its own log. No frontend turns it into "3
/// notes out of sync" — a stale index is the app's problem, not a chore
/// assigned to the reader, and there is no screen in this repo that counts what
/// the user has not done.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VaultReconcile {
    /// Notes whose file was looked at.
    pub checked: usize,
    /// Notes whose index was behind their file and has caught up.
    pub reindexed: usize,
    /// Notes whose file was not there. Nothing was written for any of them.
    pub absent: usize,
}

/// A note file's body, or `None` when there is no file there.
///
/// A read error that is *not* absence propagates: a permission problem is worth
/// hearing about, while a file that is simply gone is an ordinary state of the
/// vault (see [`reconcile_vault`]).
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

/// Bring one note's index in line with a body read from its file, and say
/// whether that changed anything.
///
/// **The one place a note's index catches up with its file** — the watcher, the
/// sweep and [`crate::Engine::refresh_note_from_disk`] all come through here,
/// so none of them can hold a different opinion about what a change is.
///
/// The comparison against what is already indexed is what makes seeing our own
/// writes free rather than merely harmless: `create_note` and
/// `update_note_body` write the file *and* the index, and the watcher then sees
/// the file event they caused. Without this the echo is one extra pointless
/// transaction per save; with it, it is one read and one `SELECT`. It also
/// absorbs the ordinary editor no-op — Obsidian rewrites a file on focus loss
/// whether or not a character changed.
///
/// Compared **trim-end**: an editor that adds a trailing newline on save has
/// not changed what the note says, and treating it as a change would re-index
/// the whole vault the first time one ran over it.
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

/// A path under the vault, as `notes.file_path` spells it.
///
/// `None` for anything that is not a markdown file inside the vault — an
/// editor's swap file, `.obsidian/workspace.json`, a directory. Components are
/// joined with `/` rather than the platform separator because that is what
/// `create_note` wrote, and a lookup that spelled it the other way would find
/// nothing on Windows and never say why.
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

/// Walk every note and bring the ones whose file has moved ahead back in line.
///
/// **This is the half a watcher cannot do**, and the reason it exists beside
/// one rather than instead of one: a watcher only ever sees the present, and
/// the ordinary case is a note edited in Obsidian on Tuesday and searched for
/// in bookmole on Thursday. It is also the whole of the answer on a
/// platform where `notify` will not start: an absent feature is a first-class
/// answer, never a failure, and an unwatchable directory must still hold a
/// working vault.
///
/// **Two stages, the pattern `sidecar_seen` established.** `stat` first and
/// skip when the file is no newer than the index; only then read and compare.
/// One caveat, stated rather than papered over: `notes.last_modified` is unix
/// *seconds*, so an external edit landing in the same second as one of our own
/// index writes is skipped by this sweep. The watcher is what covers a live
/// edit; this covers a cold one, and a cold one is never in the same second.
pub(crate) async fn reconcile_vault(storage: &Storage, vault: &Path) -> Result<VaultReconcile> {
    let mut report = VaultReconcile::default();
    for note in storage.note_files().await? {
        let path = vault.join(&note.file_path);
        report.checked += 1;

        let Ok(meta) = std::fs::metadata(&path) else {
            // Absent, unreadable, or a directory where a note should be. None
            // of those is a deletion — see `VaultWatcher`: absence is never
            // destructive.
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
    // The path is deliberately not in here: a note's filename is its slugified
    // title, which `derive_title` takes from the first six words of the body.
    // A vault path is the user's prose and belongs no higher than `trace!`.
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
        // NOCASE, as `write_links` resolved the edge.
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
        // Link near the head: only the tail goes.
        let tail = "é".repeat(400);
        let body = format!("[[X]] {tail}");
        let p = link_paragraph(&body, "X").unwrap();
        assert!(p.truncated && !p.truncated_start);
        assert!(p.text.len() <= LINK_PARAGRAPH_CAP);
        assert_eq!(p.link_range, (0, 5));

        // Link deep in: both ends go, the link is kept whole.
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
        // Reassembling with a new body keeps the header verbatim.
        assert_eq!(
            format!("{header}rewritten\n"),
            "---\npage: 5\nkind: note\n---\n\nrewritten\n"
        );

        // No frontmatter: empty header, whole content is body.
        let (h2, b2) = frontmatter_and_body("just text\n");
        assert_eq!(h2, "");
        assert_eq!(b2, "just text\n");
    }

    /// A markdown horizontal rule is `----`, and a bare `find("\n---")` matches
    /// its first four bytes. Taking that as the closing fence swallows part of
    /// the user's note into the header — and `update_note_body` then writes the
    /// mangled split back to disk, so the damage is permanent.
    #[test]
    fn a_horizontal_rule_in_the_body_is_not_mistaken_for_the_closing_fence() {
        let content = "---\nkind: note\n---\n\nIntro paragraph.\n\n----\n\nAfter the rule.\n";
        let (header, body) = frontmatter_and_body(content);

        assert_eq!(header, "---\nkind: note\n---\n\n");
        assert_eq!(body, "Intro paragraph.\n\n----\n\nAfter the rule.\n");

        // Same for a fence-like line that merely starts with three dashes.
        let dashed = "---\nkind: note\n---\n\nSee ---foo below.\n";
        let (h, b) = frontmatter_and_body(dashed);
        assert_eq!(h, "---\nkind: note\n---\n\n");
        assert_eq!(b, "See ---foo below.\n");
    }

    /// Found by the partition property below, shrunk to this: the closing
    /// fence of an *empty* frontmatter block sits on the first line, where a
    /// search for "\n---" cannot see it — so the whole file read as body.
    #[test]
    fn an_empty_frontmatter_block_is_still_frontmatter() {
        let (h, b) = frontmatter_and_body("---\n---\n\nbody text\n");
        assert_eq!(h, "---\n---\n\n");
        assert_eq!(b, "body text\n");

        // And with nothing after it at all.
        let (h2, b2) = frontmatter_and_body("---\n---\n");
        assert_eq!(h2, "---\n---\n");
        assert_eq!(b2, "");
    }

    /// Whatever the input, the two halves must reassemble into exactly the
    /// original. `update_note_body` rewrites `header + new_body`, so any byte
    /// lost here is a byte lost from someone's note file.
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
        }; // an edited number survives
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
        let vault = std::env::temp_dir().join(format!("rb-vault-test-{}", std::process::id()));
        std::fs::remove_dir_all(&vault).ok();

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

        std::fs::remove_dir_all(&vault).ok();
    }

    #[tokio::test]
    async fn anchor_lands_in_frontmatter_and_db() {
        let vault = std::env::temp_dir().join(format!("rb-vault-anchor-{}", std::process::id()));
        std::fs::remove_dir_all(&vault).ok();

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

        std::fs::remove_dir_all(&vault).ok();
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// The strongest thing that can be said about the split, and the one
        /// that protects the user's writing: `update_note_body` rewrites
        /// `header + new_body`, so any byte this loses is a byte lost from a
        /// note file on disk.
        #[test]
        fn the_split_is_always_a_partition(content in ".{0,200}") {
            let (h, b) = frontmatter_and_body(&content);
            prop_assert_eq!(format!("{h}{b}"), content.clone());
        }

        /// Generated note-shaped input, so the property exercises the real
        /// parsing path rather than mostly hitting the no-frontmatter early
        /// return.
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

        /// `slugify` names a file on disk. Empty, or `.`/`..`, or something
        /// with a separator in it would be a broken or dangerous path.
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
            // The 60 cap is a *byte* check applied after pushing, so multibyte
            // input can overshoot by up to one character's width. Assert the
            // real bound rather than a 60 that would be wrong.
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

        /// Whatever the body, a found paragraph is a substring-shaped window
        /// that holds the link token on char boundaries, within the cap
        /// (plus at most one char of slack for the link itself).
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

        /// A list row's excerpt: never panics, always a prefix of the plain text cut
        /// on a char boundary, never longer than the cap.
        #[test]
        fn an_excerpt_is_a_bounded_prefix(body in "(?s).{0,700}") {
            let e = excerpt(&body);
            let plain = plain_text(&body);
            prop_assert!(e.len() <= EXCERPT_CAP, "{} bytes", e.len());
            prop_assert!(plain.is_char_boundary(e.len()));
            prop_assert_eq!(&plain[..e.len()], e.as_str());
            prop_assert!(e.len() == plain.len() || plain.len() > EXCERPT_CAP);
        }

        /// Parts: `parse(render(parts)) == parts`, over normal part texts.
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

        /// Parts: continuing never changes the text of an earlier part, whatever
        /// the body was — and adds exactly one.
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

        /// A mention found is always on char boundaries, matches the phrase
        /// case-blind, and linking it yields a body whose links include it.
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
