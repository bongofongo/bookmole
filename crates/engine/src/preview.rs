//! Preview (macOS): the annotations inside a PDF, imported as highlights.
//!
//! Import is one-way. bookmole reads a PDF and never writes into it.
//! Everything the file says (the words under a highlight, its note, its colour,
//! where it is) belongs to the file, and goes through the shared path
//! [`Storage::upsert_imported_highlight`] with `origin = preview`.
//!
//! ## What is read
//!
//! Each page's `/Annots`: `Highlight`, `Underline`, `StrikeOut` (text markup)
//! and `Text` (a sticky note) and `FreeText`. Everything else (links, widgets,
//! ink, popups) is not a highlight and is ignored without a count.
//!
//! * `/Contents` is the note ([`ImportedHighlight::origin_note`]).
//! * `/M` is `made_at`, normalised to UTC `YYYY-MM-DD HH:MM:SS` when it parses
//!   (the unit KOReader's stamp is in), else the raw text.
//! * `/T` (the author) is **dropped**: no column holds a per-highlight author
//!   and inventing one is a migration, which this item does not carry.
//! * `/C` becomes `#rrggbb` (gray, RGB and CMYK are folded to RGB).
//! * The kind (highlight / underline / strike-out / note / free text) is kept
//!   as a tag inside `pos0`, since no other column holds it.
//!
//! ## The words are not reliably stored
//!
//! A markup annotation does not contain the words it marks; they are in the
//! page's content stream under the `/QuadPoints`. This module interprets the
//! content stream (text positioning operators only) and takes the glyphs whose
//! centres fall inside a quad. Extraction is **refused rather than guessed**:
//! a run in a font whose advance widths are not known (`/Widths`, or Courier's
//! fixed 600) that sits on a quad's line makes that annotation
//! `text_unavailable`, and so does an extraction that finds nothing. Such a
//! highlight is kept, with empty text, because its place and note are still
//! real; wrong words shown as the user's passage would be worse than none.
//!
//! A sticky note or free text has no passage. It is a **note-only row**:
//! `text = ""`, `text_unavailable = false` (nothing is missing), and the note
//! carries it. One with no `/Contents` is skipped and counted in `unparsed`.
//!
//! ## Position
//!
//! `pos_kind = pdf_rect`, `page` = the 1-based page the source put it on, and
//! `pos0 = "p{page}:{tag}:{n1},{n2},..."` where `tag` is `h`ighlight,
//! `u`nderline, `s`trike-out, `t`ext note or `f`ree text and the numbers are
//! the annotation's `/QuadPoints` (8 per quad, in the file's order) or, for a
//! note/free text, its `/Rect` (4), in PDF user-space points, at most two
//! decimals with trailing zeros trimmed. Never converted to another unit.
//!
//! ## Identity
//!
//! `/NM` (the annotation's unique name) is the `origin_id` when present and not
//! repeated in the file. Otherwise `identity = "{page}|{tag}|{llx},{lly},{urx},{ury}"`
//! with the `/Rect` (the quads' bounding box if there is no `/Rect`) rounded to
//! whole points, so sub-point jitter between re-reads does not change it. Two
//! annotations that would share an identity get a `|#n` counter in file order.
//! Nothing the app rewrites in place (the note, the colour) is in it.
//!
//! Both keys are facts about **one file**, so every highlight is built with
//! [`IdentityScope::Copy`]: two PDF copies of a book annotated at the same
//! place (the same `/NM`, or the same page and rect) are two highlights, each
//! found, refreshed and removed only by its own copy's import. The storage
//! module doc (`storage/imported_highlights.rs`, "Scope") is how.
//!
//! ## Deletions
//!
//! Deleting an annotation in Preview deletes it from the file, so the file no
//! longer has it. A re-import removes this file's rows (`copy_id` equal to the
//! one imported and `origin = preview`, so another copy's rows are never
//! touched) that the file no longer holds, **except** any that carry something
//! of bookmole's own (an `annotation`, an anchored note, a flashcard, a
//! citation, a card passage): those are kept and counted in a
//! [`DiagnosticKind::PdfAnnotationsKept`]. Nothing is removed when there is no
//! telling which file a row came from: no `copy_id`, or a copy with **more than
//! one `path` place** (two files merged into one copy by `merge_copies`, or one
//! file in two folders that have since diverged). A file that could not be
//! read removes nothing, and neither does one that was read **incompletely**
//! ([`PdfScan::is_complete`]: a cap reached, a page not all read, anything
//! unparsed): a row the pass did not see may be one it did not read, so the
//! removal is skipped and a [`DiagnosticKind::PdfScanIncomplete`] says so.
//!
//! ## Hostile files
//!
//! Every loop is bounded by a named cap, never by a count the file states:
//! annotations, quads per annotation and per file, glyphs and unmeasured runs
//! per page, `q` nesting, decoded content per page and per file, decoded font
//! encodings, and the glyph visits extraction may make. Strings taken from the
//! file (`/Contents`, `/NM`, `/M`) are cut to a byte budget **before** they are
//! decoded. Coordinates must be finite and lie within the page's `/MediaBox`
//! widened by a page on every side (and within [`MAX_ABS_COORD`]); an
//! annotation's quads are all-or-nothing, so a single bad quad drops them all
//! (the annotation keeps its `/Rect`, or is counted `unparsed`). A cap that is
//! reached never produces guessed words: the highlights it affects are kept as
//! `text_unavailable`, or counted `unparsed` when nothing of them is left.
//!
//! Highlight text and notes are the user's private reading: nothing here logs
//! them above `trace!`, and nothing here logs them at all.
//!
//! [`DiagnosticKind::PdfAnnotationsKept`]: crate::DiagnosticKind::PdfAnnotationsKept
//! [`DiagnosticKind::PdfScanIncomplete`]: crate::DiagnosticKind::PdfScanIncomplete

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lopdf::content::Content;
use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId};

use crate::copies::PlaceKind;
use crate::diagnostic::{Diagnostic, ErrorClass};
use crate::error::{EngineError, Result};
use crate::highlight_origin::{
    HighlightImportSummary, HighlightOrigin, IdentityScope, ImportedHighlight, ImportedOutcome,
    PosKind,
};
use crate::storage::Storage;

/// A PDF larger than this is not read.
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Most any single stream may inflate to while loading.
const MAX_STREAM_BYTES: usize = 64 * 1024 * 1024;
/// Most one page's content may inflate to for reading its words. A page of
/// text is tens of KB and a page of dense vector art a few MB; `Content::decode`
/// holds every operation in memory, so this is also a memory bound. Past it the
/// page's words are unavailable.
const MAX_PAGE_CONTENT_BYTES: usize = 8 * 1024 * 1024;
/// Most page content one file may inflate, across all its pages. Pages may
/// share one content stream and each decodes it again, so the per-page cap alone
/// does not bound a file.
const MAX_FILE_CONTENT_BYTES: usize = 128 * 1024 * 1024;
/// Most a simple font's encoding stream (a `ToUnicode` CMap, at most 256 codes,
/// a few KB in practice) may inflate to.
const MAX_FONT_BYTES: usize = 256 * 1024;
/// Most font-encoding bytes one file may inflate, across its fonts.
const MAX_FILE_FONT_BYTES: usize = 64 * 1024 * 1024;
/// Annotations read from one file.
const MAX_ANNOTATIONS: usize = 20_000;
/// Quads read from one annotation.
const MAX_QUADS: usize = 256;
/// Quads read from one file, across its annotations. Each quad is eight stored
/// numbers in `pos0` and one pass over its line's glyphs, and without this the
/// two caps above multiply (20 000 × 256 = 5 million quads). A heavily
/// annotated real book has a few thousand highlights of one to three lines, so
/// this is ten times that, and bounds a file's `pos0` at about 4 MB. An
/// annotation that would cross it is skipped and counted in `unparsed`.
const MAX_FILE_QUADS: usize = 50_000;
/// Glyphs collected from one page.
const MAX_GLYPHS: usize = 400_000;
/// Text runs in an unmeasurable font recorded from one page.
const MAX_UNMEASURED: usize = 100_000;
/// `q` nesting one page may reach (Acrobat's own limit is 28).
const MAX_GSTATE_DEPTH: usize = 256;
/// Glyph and run visits one file's extraction may make. Glyphs are searched by
/// line, so a real highlight visits about one line (~100); this only bites when
/// a file stacks thousands of quads over one enormous line. An annotation that
/// would cross it gets `text_unavailable`.
const MAX_EXTRACTION_WORK: usize = 50_000_000;
/// The longest note or extracted passage kept, in bytes.
const MAX_TEXT_BYTES: usize = 64 * 1024;
/// The longest `/NM` taken as an identity, in bytes. A real one is a UUID or
/// shorter; a longer one is ignored (not truncated: two truncated names could
/// collide) and the annotation falls back to its place.
const MAX_NAME_BYTES: usize = 256;
/// The raw bytes of `/M` read. A PDF date is at most ~24 characters.
const MAX_DATE_BYTES: usize = 128;
/// No coordinate is further than this from the origin, in points (Acrobat's
/// largest page is 14 400). Bounds every number in `pos0` to nine characters.
const MAX_ABS_COORD: f64 = 100_000.0;
/// The longest `pos0` an annotation produces. With coordinates bounded by
/// [`MAX_ABS_COORD`], [`MAX_QUADS`] quads never reach it from a scan; it is the
/// guard for a [`PdfScan`] built by hand, and drops whole trailing quads.
const MAX_POS0_BYTES: usize = 24 * 1024;

/// What kind of PDF annotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotKind {
    Highlight,
    Underline,
    StrikeOut,
    /// A sticky note.
    Text,
    FreeText,
}

impl AnnotKind {
    fn from_subtype(s: &[u8]) -> Option<AnnotKind> {
        Some(match s {
            b"Highlight" => AnnotKind::Highlight,
            b"Underline" => AnnotKind::Underline,
            b"StrikeOut" => AnnotKind::StrikeOut,
            b"Text" => AnnotKind::Text,
            b"FreeText" => AnnotKind::FreeText,
            _ => return None,
        })
    }

    /// The tag inside `pos0` and the identity.
    pub fn tag(self) -> char {
        match self {
            AnnotKind::Highlight => 'h',
            AnnotKind::Underline => 'u',
            AnnotKind::StrikeOut => 's',
            AnnotKind::Text => 't',
            AnnotKind::FreeText => 'f',
        }
    }

    fn is_markup(self) -> bool {
        matches!(
            self,
            AnnotKind::Highlight | AnnotKind::Underline | AnnotKind::StrikeOut
        )
    }
}

/// One annotation as read from the file, before it becomes a highlight.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfAnnotation {
    /// 1-based.
    pub page: u32,
    pub kind: AnnotKind,
    /// `/NM`.
    pub name: Option<String>,
    pub rect: Option<[f64; 4]>,
    pub quads: Vec<[f64; 8]>,
    /// `/Contents`.
    pub contents: Option<String>,
    /// `/M`, normalised.
    pub made_at: Option<String>,
    /// `/C` as `#rrggbb`.
    pub color: Option<String>,
    /// The words under the quads; `None` when they could not be extracted.
    pub text: Option<String>,
}

/// Everything readable in one PDF.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PdfScan {
    pub annotations: Vec<PdfAnnotation>,
    /// Annotations of a handled type that could not be made into a highlight.
    pub unparsed: usize,
    /// A cap was reached somewhere in the file: the annotation or quad budget,
    /// a page whose content could not all be read (`broken`), or the file's
    /// content, font or extraction budget. Together with `unparsed`, this is
    /// what [`PdfScan::is_complete`] reads.
    pub truncated: bool,
}

impl PdfScan {
    /// Whether this scan is an **authoritative list** of the file's
    /// annotations: nothing was skipped, nothing unparsed, no cap reached. Only
    /// a complete scan may remove rows the file "no longer holds" — after a cut
    /// short one, a row not seen may simply not have been read.
    pub fn is_complete(&self) -> bool {
        !self.truncated && self.unparsed == 0
    }
}

// ---- reading the file ---------------------------------------------------------

/// Read every annotation of every page. `Err` is a typed [`Diagnostic`] (not a
/// PDF, truncated, encrypted, too large), never a panic.
pub fn scan_pdf_annotations(path: &Path) -> std::result::Result<PdfScan, Diagnostic> {
    let unreadable = |class: ErrorClass, detail: String| {
        Diagnostic::pdf_unreadable(path.to_path_buf(), class, detail)
    };
    let meta = std::fs::metadata(path)
        .map_err(|e| unreadable(ErrorClass::Io, format!("cannot read: {}", e.kind())))?;
    if !meta.is_file() {
        return Err(unreadable(ErrorClass::Io, "not a file".into()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(unreadable(ErrorClass::Other, "too large to read".into()));
    }
    let options = LoadOptions {
        max_decompressed_size: Some(MAX_STREAM_BYTES),
        ..Default::default()
    };
    // A hostile file must not take the pane down: lopdf is a parser of untrusted
    // bytes, and a panic inside it becomes a diagnostic like any other failure.
    let loaded = std::panic::catch_unwind(|| Document::load_with_options(path, options));
    let doc = match loaded {
        Ok(Ok(doc)) => doc,
        Ok(Err(e)) => {
            let text = e.to_string().to_ascii_lowercase();
            if text.contains("encrypt") || text.contains("password") || text.contains("decrypt") {
                return Err(Diagnostic::pdf_encrypted(path.to_path_buf()));
            }
            return Err(unreadable(ErrorClass::Parse, "not a readable PDF".into()));
        }
        Err(_) => return Err(unreadable(ErrorClass::Parse, "malformed PDF".into())),
    };
    if doc.trailer.has(b"Encrypt") && !doc.was_encrypted() {
        return Err(Diagnostic::pdf_encrypted(path.to_path_buf()));
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scan_document(&doc))) {
        Ok(scan) => Ok(scan),
        Err(_) => Err(unreadable(ErrorClass::Parse, "malformed PDF".into())),
    }
}

/// What one file may still spend. Every cap that spans pages lives here.
struct Budget {
    quads: usize,
    work: usize,
    content_bytes: usize,
    font_bytes: usize,
}

impl Budget {
    fn new() -> Budget {
        Budget {
            quads: MAX_FILE_QUADS,
            work: MAX_EXTRACTION_WORK,
            content_bytes: MAX_FILE_CONTENT_BYTES,
            font_bytes: MAX_FILE_FONT_BYTES,
        }
    }
}

fn scan_document(doc: &Document) -> PdfScan {
    let mut scan = PdfScan::default();
    let mut budget = Budget::new();
    let mut fonts = FontCache::default();
    for (page_no, page_id) in doc.get_pages() {
        let annots = page_annotations(doc, page_id);
        if annots.is_empty() {
            continue;
        }
        let bounds = page_bounds(doc, page_id);
        let mut glyphs: Option<PageGlyphs> = None;
        for dict in annots {
            let Some(kind) = dict
                .get(b"Subtype")
                .ok()
                .and_then(|o| deref(doc, o))
                .and_then(|o| o.as_name().ok())
                .and_then(AnnotKind::from_subtype)
            else {
                continue;
            };
            if scan.annotations.len() >= MAX_ANNOTATIONS {
                scan.unparsed += 1;
                scan.truncated = true;
                continue;
            }
            let rect = dict
                .get(b"Rect")
                .ok()
                .and_then(|o| number_array(doc, o, 4))
                .map(|v| [v[0], v[1], v[2], v[3]])
                .filter(|r| bounds.holds_all(r));
            let mut quads: Vec<[f64; 8]> = Vec::new();
            if kind.is_markup() {
                if let Some(v) = dict
                    .get(b"QuadPoints")
                    .ok()
                    .and_then(|o| number_array(doc, o, 0))
                {
                    quads = v
                        .chunks_exact(8)
                        .take(MAX_QUADS)
                        .map(|c| [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]])
                        .collect();
                }
                // All or nothing: one quad off the page makes the rest suspect.
                if !quads.iter().all(|q| bounds.holds_all(q)) {
                    quads.clear();
                }
                if quads.len() > budget.quads {
                    scan.unparsed += 1;
                    scan.truncated = true;
                    continue;
                }
                budget.quads -= quads.len();
            }
            let contents = dict
                .get(b"Contents")
                .ok()
                .and_then(|o| deref(doc, o))
                .and_then(string_of)
                .map(|s| cap_text(&pdf_text(head(s, 2 * MAX_TEXT_BYTES + 3))))
                .filter(|s| !s.trim().is_empty());
            let text = if kind.is_markup() && !quads.is_empty() {
                let g = glyphs
                    .get_or_insert_with(|| page_glyphs(doc, page_id, &mut fonts, &mut budget));
                scan.truncated |= g.broken;
                text_under(g, &quads, &mut budget.work)
            } else {
                None
            };
            // Handled types that carry nothing to make a highlight from.
            let placeless = if kind.is_markup() {
                quads.is_empty() && rect.is_none()
            } else {
                contents.is_none()
            };
            if placeless {
                scan.unparsed += 1;
                continue;
            }
            scan.annotations.push(PdfAnnotation {
                page: page_no,
                kind,
                name: dict
                    .get(b"NM")
                    .ok()
                    .and_then(|o| deref(doc, o))
                    .and_then(string_of)
                    .filter(|b| b.len() <= MAX_NAME_BYTES)
                    .map(pdf_text)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
                rect,
                quads,
                contents,
                made_at: dict
                    .get(b"M")
                    .ok()
                    .and_then(|o| deref(doc, o))
                    .and_then(string_of)
                    .map(|b| pdf_date(&pdf_text(head(b, MAX_DATE_BYTES)))),
                color: dict
                    .get(b"C")
                    .ok()
                    .and_then(|o| number_array(doc, o, 0))
                    .and_then(|v| color_hex(&v)),
                text,
            });
        }
    }
    // A file-wide budget spent to the last byte means something was refused
    // for want of it (every overspend zeroes or drains its budget).
    scan.truncated |= budget.work == 0 || budget.content_bytes == 0 || budget.font_bytes == 0;
    tracing::debug!(
        annotations = scan.annotations.len(),
        unparsed = scan.unparsed,
        truncated = scan.truncated,
        "read pdf annotations"
    );
    scan
}

/// The annotation dictionaries of one page, whether `/Annots` is an array or a
/// reference to one, and whether its members are references or direct.
fn page_annotations(doc: &Document, page_id: ObjectId) -> Vec<&Dictionary> {
    let Ok(page) = doc.get_dictionary(page_id) else {
        return Vec::new();
    };
    let Some(Object::Array(items)) = page.get(b"Annots").ok().and_then(|o| deref(doc, o)) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|o| deref(doc, o))
        .filter_map(|o| o.as_dict().ok())
        .collect()
}

fn deref<'a>(doc: &'a Document, o: &'a Object) -> Option<&'a Object> {
    doc.dereference(o).ok().map(|(_, o)| o)
}

fn string_of(o: &Object) -> Option<&[u8]> {
    match o {
        Object::String(b, _) => Some(b),
        _ => None,
    }
}

/// At most the first `n` bytes: a string from the file is cut **before** it is
/// decoded, so its length never sizes an allocation.
fn head(b: &[u8], n: usize) -> &[u8] {
    &b[..b.len().min(n)]
}

/// Where an annotation's coordinates may lie on one page.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Bounds {
    const ALL: Bounds = Bounds {
        x0: -MAX_ABS_COORD,
        y0: -MAX_ABS_COORD,
        x1: MAX_ABS_COORD,
        y1: MAX_ABS_COORD,
    };

    /// Every `(x, y)` pair of a flat coordinate list is finite and inside.
    fn holds_all(&self, v: &[f64]) -> bool {
        v.chunks_exact(2).all(|p| {
            let (x, y) = (p[0], p[1]);
            x.is_finite()
                && y.is_finite()
                && (self.x0..=self.x1).contains(&x)
                && (self.y0..=self.y1).contains(&y)
        })
    }
}

/// A page attribute, inherited through `/Parent` (as `/MediaBox` may be).
fn inherited<'a>(doc: &'a Document, page_id: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut dict = doc.get_dictionary(page_id).ok()?;
    // The page tree is shallow; a deeper chain is a loop or an attack.
    for _ in 0..32 {
        if let Ok(o) = dict.get(key) {
            return deref(doc, o);
        }
        dict = dict
            .get(b"Parent")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_dict().ok())?;
    }
    None
}

/// The page's `/MediaBox` widened by its own larger side on every side, and
/// never past [`MAX_ABS_COORD`]. No usable box means [`Bounds::ALL`].
fn page_bounds(doc: &Document, page_id: ObjectId) -> Bounds {
    let Some(b) = inherited(doc, page_id, b"MediaBox").and_then(|o| number_array(doc, o, 4)) else {
        return Bounds::ALL;
    };
    if b.iter().any(|v| v.abs() > MAX_ABS_COORD) {
        return Bounds::ALL;
    }
    let (x0, x1) = (b[0].min(b[2]), b[0].max(b[2]));
    let (y0, y1) = (b[1].min(b[3]), b[1].max(b[3]));
    let margin = (x1 - x0).max(y1 - y0);
    if margin <= 0.0 {
        return Bounds::ALL;
    }
    Bounds {
        x0: (x0 - margin).max(-MAX_ABS_COORD),
        y0: (y0 - margin).max(-MAX_ABS_COORD),
        x1: (x1 + margin).min(MAX_ABS_COORD),
        y1: (y1 + margin).min(MAX_ABS_COORD),
    }
}

fn number(doc: &Document, o: &Object) -> Option<f64> {
    match deref(doc, o)? {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(f64::from(*r)),
        _ => None,
    }
}

/// A flat array of finite numbers, at most `MAX_QUADS * 8` of them; `len` 0
/// means any length. All-or-nothing: one non-finite member rejects the array.
fn number_array(doc: &Document, o: &Object, len: usize) -> Option<Vec<f64>> {
    let Object::Array(items) = deref(doc, o)? else {
        return None;
    };
    if len != 0 && items.len() != len {
        return None;
    }
    let v: Option<Vec<f64>> = items
        .iter()
        .take(MAX_QUADS * 8)
        .map(|i| number(doc, i).filter(|n| n.is_finite()))
        .collect();
    v.filter(|v| !v.is_empty())
}

/// A PDF text string: UTF-16BE with a BOM, UTF-8 with a BOM, else PDFDocEncoding
/// (read here as Latin-1, which is exact for the printable range).
fn pdf_text(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    b.iter().map(|&c| c as char).collect()
}

fn cap_text(s: &str) -> String {
    if s.len() <= MAX_TEXT_BYTES {
        return s.to_string();
    }
    let mut end = MAX_TEXT_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// `/C` to `#rrggbb`. Gray (1), RGB (3) and CMYK (4) are folded to RGB; an empty
/// array (transparent) or any other length is no colour.
fn color_hex(v: &[f64]) -> Option<String> {
    let c = |x: f64| x.clamp(0.0, 1.0);
    let (r, g, b) = match v {
        [g] => (c(*g), c(*g), c(*g)),
        [r, g, b] => (c(*r), c(*g), c(*b)),
        [cy, m, y, k] => (
            (1.0 - c(*cy)) * (1.0 - c(*k)),
            (1.0 - c(*m)) * (1.0 - c(*k)),
            (1.0 - c(*y)) * (1.0 - c(*k)),
        ),
        _ => return None,
    };
    let byte = |x: f64| (x * 255.0).round() as u8;
    Some(format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b)))
}

/// `D:YYYYMMDDHHmmSSOHH'mm'` to UTC `YYYY-MM-DD HH:MM:SS`. Anything that does
/// not parse is kept as the file wrote it (capped), since the stamp is the
/// source's own.
fn pdf_date(raw: &str) -> String {
    parse_pdf_date(raw).unwrap_or_else(|| raw.trim().chars().take(64).collect())
}

fn parse_pdf_date(raw: &str) -> Option<String> {
    let s = raw.trim().strip_prefix("D:").unwrap_or(raw.trim());
    let digits = |from: usize, len: usize, default: i64| -> Option<i64> {
        match s.get(from..from + len) {
            Some(d) if d.bytes().all(|b| b.is_ascii_digit()) => d.parse().ok(),
            Some(_) => None,
            None => Some(default),
        }
    };
    if s.len() < 4 {
        return None;
    }
    let (y, mo, d) = (digits(0, 4, 0)?, digits(4, 2, 1)?, digits(6, 2, 1)?);
    let (h, mi, se) = (digits(8, 2, 0)?, digits(10, 2, 0)?, digits(12, 2, 0)?);
    let month = time::Month::try_from(u8::try_from(mo).ok()?).ok()?;
    let date = time::Date::from_calendar_date(i32::try_from(y).ok()?, month, u8::try_from(d).ok()?)
        .ok()?;
    let clock = time::Time::from_hms(
        u8::try_from(h).ok()?,
        u8::try_from(mi).ok()?,
        u8::try_from(se).ok()?,
    )
    .ok()?;
    let mut at = time::PrimitiveDateTime::new(date, clock);
    // Offset: `Z`, or `+HH'mm'` / `-HH'mm'` (the apostrophes optional).
    if let Some(tz) = s.get(14..).filter(|t| !t.is_empty()) {
        let sign = match tz.as_bytes()[0] {
            b'+' => 1,
            b'-' => -1,
            b'Z' | b'z' => 0,
            _ => return None,
        };
        if sign != 0 {
            let nums: Vec<i64> = tz[1..]
                .split(|c: char| !c.is_ascii_digit())
                .filter(|p| !p.is_empty())
                .filter_map(|p| p.parse().ok())
                .collect();
            let (oh, om) = match nums.as_slice() {
                [hm] if *hm >= 100 => (hm / 100, hm % 100),
                [hh] => (*hh, 0),
                [hh, mm, ..] => (*hh, *mm),
                [] => (0, 0),
            };
            if oh > 23 || om > 59 {
                return None;
            }
            at -= time::Duration::minutes(sign * (oh * 60 + om));
        }
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    ))
}

// ---- the words under a quad ---------------------------------------------------

struct Glyph {
    /// Centre of the glyph's advance, page space.
    cx: f64,
    /// Vertical middle of the glyph, page space.
    cy: f64,
    /// Order in the content stream, which is reading order within a line.
    seq: u32,
    ch: char,
}

#[derive(Default)]
struct PageGlyphs {
    /// Sorted by `cy` once the page is read.
    glyphs: Vec<Glyph>,
    /// `(x of the run's start, y)` of every run whose advance widths are not
    /// known: text is there, but where each letter ends is not. Sorted by `y`.
    unmeasured: Vec<(f64, f64)>,
    /// The content stream could not be read or decoded at all, or a cap was
    /// reached part-way: either way some of the page's words were not read, so
    /// none of its quads can be said to hold all of theirs.
    broken: bool,
}

#[derive(Clone, Copy)]
struct Mat([f64; 6]);

impl Mat {
    const ID: Mat = Mat([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    fn translate(x: f64, y: f64) -> Mat {
        Mat([1.0, 0.0, 0.0, 1.0, x, y])
    }
    /// `self` applied first, then `o` (row-vector convention).
    fn then(self, o: Mat) -> Mat {
        let (a, b) = (self.0, o.0);
        Mat([
            a[0] * b[0] + a[1] * b[2],
            a[0] * b[1] + a[1] * b[3],
            a[2] * b[0] + a[3] * b[2],
            a[2] * b[1] + a[3] * b[3],
            a[4] * b[0] + a[5] * b[2] + b[4],
            a[4] * b[1] + a[5] * b[3] + b[5],
        ])
    }
    fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (
            x * self.0[0] + y * self.0[2] + self.0[4],
            x * self.0[1] + y * self.0[3] + self.0[5],
        )
    }
}

struct FontInfo<'a> {
    encoding: Option<lopdf::Encoding<'a>>,
    /// Advance in 1/1000 em by code, when the font says (or is Courier).
    widths: Option<Widths>,
}

impl FontInfo<'_> {
    const UNMEASURED: FontInfo<'static> = FontInfo {
        encoding: None,
        widths: None,
    };
}

enum Widths {
    Fixed(f64),
    Table {
        first: i64,
        widths: Vec<f64>,
        missing: f64,
    },
}

impl Widths {
    fn of(&self, code: u8) -> f64 {
        match self {
            Widths::Fixed(w) => *w,
            Widths::Table {
                first,
                widths,
                missing,
            } => {
                let i = i64::from(code) - first;
                usize::try_from(i)
                    .ok()
                    .and_then(|i| widths.get(i).copied())
                    .unwrap_or(*missing)
            }
        }
    }
}

/// Fonts read once per file, keyed by the font dictionary's address (pages
/// share font objects, and a page names them under its own resource names).
/// Read only when a `Tf` selects them.
#[derive(Default)]
struct FontCache<'a> {
    infos: Vec<FontInfo<'a>>,
    by_dict: HashMap<usize, usize>,
}

impl<'a> FontCache<'a> {
    fn index(&mut self, doc: &'a Document, dict: &'a Dictionary, budget: &mut Budget) -> usize {
        let key = dict as *const Dictionary as usize;
        if let Some(&i) = self.by_dict.get(&key) {
            return i;
        }
        let info = font_info(doc, dict, budget);
        self.infos.push(info);
        self.by_dict.insert(key, self.infos.len() - 1);
        self.infos.len() - 1
    }
}

fn font_info<'a>(doc: &'a Document, dict: &'a Dictionary, budget: &mut Budget) -> FontInfo<'a> {
    let subtype = dict
        .get(b"Subtype")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_name().ok());
    // A composite font's advances are not read, so its runs are unmeasured
    // whatever its encoding; do not decode a CMap for nothing.
    if subtype == Some(b"Type0") {
        return FontInfo::UNMEASURED;
    }
    let base = dict
        .get(b"BaseFont")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_name().ok())
        .unwrap_or(b"");
    let widths =
        if let Some(Object::Array(ws)) = dict.get(b"Widths").ok().and_then(|o| deref(doc, o)) {
            let first = dict
                .get(b"FirstChar")
                .ok()
                .and_then(|o| number(doc, o))
                .filter(|n| n.is_finite())
                .map_or(0, |n| n as i64);
            let table: Vec<f64> = ws
                .iter()
                .take(256)
                .map(|w| number(doc, w).filter(|n| n.is_finite()).unwrap_or(0.0))
                .collect();
            let missing = dict
                .get(b"FontDescriptor")
                .ok()
                .and_then(|o| deref(doc, o))
                .and_then(|o| o.as_dict().ok())
                .and_then(|d| d.get(b"MissingWidth").ok())
                .and_then(|o| number(doc, o))
                .filter(|n| n.is_finite())
                .unwrap_or(0.0);
            Some(Widths::Table {
                first,
                widths: table,
                missing,
            })
        } else if base.starts_with(b"Courier") || base.windows(8).any(|w| w == b"+Courier") {
            // A standard-14 monospaced font needs no /Widths: every glyph is 600.
            Some(Widths::Fixed(600.0))
        } else {
            None
        };
    if widths.is_none() {
        return FontInfo::UNMEASURED;
    }
    // Every stream lopdf could inflate to build the encoding, probed (inflated
    // once under the file's budget) before lopdf is allowed near it:
    //
    // * `/ToUnicode`, which lopdf 0.44 decodes when there is no usable
    //   `/Encoding` **and** when `/Encoding` is the name `Identity-H` /
    //   `Identity-V` (`Dictionary::base_encoding`). The second case is the one
    //   the first cut missed: it probed `/ToUnicode` only when `/Encoding` was
    //   absent, so an Identity-named simple font had lopdf inflate a stream the
    //   budget never charged (found in a security review).
    // * an `/Encoding` that is itself a stream (a CMap). lopdf 0.44 does not
    //   decode one, but charging for it costs a few KB on a real file and keeps
    //   the bound true if a later lopdf starts to.
    //
    // A name or a `/Differences` dictionary inflates nothing.
    let encoding_entry = dict.get(b"Encoding").ok().and_then(|o| deref(doc, o));
    let to_unicode_decodable = match encoding_entry {
        None => true,
        Some(Object::Name(n)) => n.as_slice() == b"Identity-H" || n.as_slice() == b"Identity-V",
        Some(_) => false,
    };
    let mut streams: Vec<&lopdf::Stream> = Vec::with_capacity(2);
    if let Some(Object::Stream(s)) = encoding_entry {
        streams.push(s);
    }
    if to_unicode_decodable
        && let Some(s) = dict
            .get(b"ToUnicode")
            .ok()
            .and_then(|o| deref(doc, o))
            .and_then(|o| o.as_stream().ok())
    {
        streams.push(s);
    }
    let mut largest = 0usize;
    for s in streams {
        let limit = MAX_FONT_BYTES.min(budget.font_bytes);
        match s.decompressed_content_with_limit(limit) {
            Ok(bytes) => {
                budget.font_bytes -= bytes.len().min(budget.font_bytes);
                largest = largest.max(bytes.len());
            }
            Err(_) => {
                budget.font_bytes -= limit;
                return FontInfo::UNMEASURED;
            }
        }
    }
    // lopdf now decodes (at most) one of the probed streams a second time, so
    // that decode is charged too, and its limit is exactly what was probed and
    // paid for: nothing lopdf inflates here is outside the per-file total.
    if largest > budget.font_bytes {
        budget.font_bytes = 0;
        return FontInfo::UNMEASURED;
    }
    budget.font_bytes -= largest;
    let encoding = dict.get_font_encoding_with_limit(doc, largest).ok();
    FontInfo { encoding, widths }
}

/// A number operand, or 0 for anything else, including a non-finite one.
fn operand(doc_ops: &[Object], i: usize) -> f64 {
    let n = match doc_ops.get(i) {
        Some(Object::Integer(n)) => *n as f64,
        Some(Object::Real(r)) => f64::from(*r),
        _ => 0.0,
    };
    if n.is_finite() { n } else { 0.0 }
}

fn page_glyphs<'a>(
    doc: &'a Document,
    page_id: ObjectId,
    cache: &mut FontCache<'a>,
    budget: &mut Budget,
) -> PageGlyphs {
    let mut out = PageGlyphs::default();
    let limit = MAX_PAGE_CONTENT_BYTES.min(budget.content_bytes);
    let Ok(bytes) = doc.get_page_content_with_limit(page_id, limit) else {
        budget.content_bytes -= limit;
        out.broken = true;
        return out;
    };
    budget.content_bytes -= bytes.len().min(budget.content_bytes);
    let Ok(content) = Content::decode(&bytes) else {
        out.broken = true;
        return out;
    };
    drop(bytes);
    // Resource names to font dictionaries; each is read on first `Tf`.
    let names: HashMap<Vec<u8>, &Dictionary> = doc
        .get_page_fonts(page_id)
        .map(|f| f.into_iter().collect())
        .unwrap_or_default();

    let mut ctm = Mat::ID;
    let mut stack: Vec<Mat> = Vec::new();
    let (mut tm, mut tlm) = (Mat::ID, Mat::ID);
    let (mut font, mut size, mut leading): (Option<usize>, f64, f64) = (None, 0.0, 0.0);
    let mut seq: u32 = 0;

    // Show one string at the current text position, advancing it. Sets
    // `out.broken` when a cap is reached, and the caller stops there.
    macro_rules! show {
        ($bytes:expr) => {{
            let bytes: &[u8] = $bytes;
            let render = tm.then(ctm);
            let (ox, oy) = render.apply(0.0, 0.0);
            let measured = font
                .map(|i| &cache.infos[i])
                .and_then(|f| f.widths.as_ref().map(|w| (f, w)));
            match measured {
                Some((f, w)) if f.encoding.is_some() => {
                    // A simple font is one byte per glyph: a run longer than
                    // what is left of the cap would cross it, so it is refused
                    // before its bytes are decoded at all.
                    if bytes.len() > MAX_GLYPHS - out.glyphs.len() {
                        out.broken = true;
                    } else {
                        let text = f
                            .encoding
                            .as_ref()
                            .and_then(|e| Document::decode_text(e, bytes).ok())
                            .unwrap_or_default();
                        // One code per character is what a simple font gives; if
                        // the decoder disagrees, the positions cannot be trusted.
                        if text.chars().count() == bytes.len() {
                            let sx = (render.0[0].powi(2) + render.0[1].powi(2)).sqrt();
                            let sy = (render.0[2].powi(2) + render.0[3].powi(2)).sqrt();
                            let mut dx = 0.0;
                            for (ch, code) in text.chars().zip(bytes) {
                                if out.glyphs.len() >= MAX_GLYPHS {
                                    out.broken = true;
                                    break;
                                }
                                let adv = w.of(*code) / 1000.0 * size;
                                let (x0, y0) = render.apply(dx, 0.0);
                                let (cx, cy) = (x0 + adv * sx / 2.0, y0 + 0.3 * size * sy);
                                // A glyph at a non-finite place is under no quad.
                                if cx.is_finite() && cy.is_finite() {
                                    out.glyphs.push(Glyph { cx, cy, seq, ch });
                                    seq = seq.wrapping_add(1);
                                }
                                dx += adv;
                            }
                            tm = Mat::translate(dx, 0.0).then(tm);
                        } else {
                            out.unmeasured.push((ox, oy + 0.3 * size));
                        }
                    }
                }
                _ => {
                    if !bytes.is_empty() {
                        out.unmeasured.push((ox, oy + 0.3 * size));
                    }
                }
            }
            if out.unmeasured.len() > MAX_UNMEASURED {
                out.broken = true;
            }
        }};
    }

    for op in &content.operations {
        let a = &op.operands;
        match op.operator.as_str() {
            "q" => {
                if stack.len() >= MAX_GSTATE_DEPTH {
                    out.broken = true;
                } else {
                    stack.push(ctm);
                }
            }
            "Q" => ctm = stack.pop().unwrap_or(ctm),
            "cm" if a.len() == 6 => {
                ctm = Mat([
                    operand(a, 0),
                    operand(a, 1),
                    operand(a, 2),
                    operand(a, 3),
                    operand(a, 4),
                    operand(a, 5),
                ])
                .then(ctm)
            }
            "BT" => {
                tm = Mat::ID;
                tlm = Mat::ID;
            }
            "Tf" if a.len() == 2 => {
                font = a[0]
                    .as_name()
                    .ok()
                    .and_then(|n| names.get(n))
                    .map(|d| cache.index(doc, d, budget));
                size = operand(a, 1);
            }
            "TL" => leading = operand(a, 0),
            "Td" | "TD" if a.len() == 2 => {
                if op.operator == "TD" {
                    leading = -operand(a, 1);
                }
                tlm = Mat::translate(operand(a, 0), operand(a, 1)).then(tlm);
                tm = tlm;
            }
            "Tm" if a.len() == 6 => {
                tlm = Mat([
                    operand(a, 0),
                    operand(a, 1),
                    operand(a, 2),
                    operand(a, 3),
                    operand(a, 4),
                    operand(a, 5),
                ]);
                tm = tlm;
            }
            "T*" => {
                tlm = Mat::translate(0.0, -leading).then(tlm);
                tm = tlm;
            }
            "Tj" => {
                if let Some(Object::String(b, _)) = a.first() {
                    show!(b);
                }
            }
            "'" | "\"" => {
                tlm = Mat::translate(0.0, -leading).then(tlm);
                tm = tlm;
                if let Some(Object::String(b, _)) = a.last() {
                    show!(b);
                }
            }
            "TJ" => {
                if let Some(Object::Array(items)) = a.first() {
                    for item in items {
                        match item {
                            Object::String(b, _) => show!(b),
                            Object::Integer(_) | Object::Real(_) => {
                                let adj = -operand(std::slice::from_ref(item), 0) / 1000.0 * size;
                                tm = Mat::translate(adj, 0.0).then(tm);
                            }
                            _ => {}
                        }
                        if out.broken {
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
        if out.broken {
            break;
        }
    }
    if out.broken {
        // Nothing on this page will be read; free it now.
        out.glyphs = Vec::new();
        out.unmeasured = Vec::new();
        return out;
    }
    out.glyphs.sort_by(|a, b| a.cy.total_cmp(&b.cy));
    out.unmeasured.sort_by(|a, b| a.1.total_cmp(&b.1));
    out
}

/// The words under a markup annotation's quads, or `None` when they cannot be
/// stated honestly (see the module doc). Spends `work` (glyphs and runs
/// visited); an annotation that would overspend it is `None` too.
fn text_under(page: &PageGlyphs, quads: &[[f64; 8]], work: &mut usize) -> Option<String> {
    if page.broken {
        return None;
    }
    let mut out = String::new();
    for q in quads {
        let xs = [q[0], q[2], q[4], q[6]];
        let ys = [q[1], q[3], q[5], q[7]];
        let (minx, maxx) = (min_of(&xs), max_of(&xs));
        let (miny, maxy) = (min_of(&ys), max_of(&ys));
        // The runs and glyphs on this quad's line, found by bisection.
        let runs = {
            let from = page.unmeasured.partition_point(|r| r.1 < miny - 1.0);
            let to = page.unmeasured.partition_point(|r| r.1 <= maxy + 1.0);
            &page.unmeasured[from..to.max(from)]
        };
        let line = {
            let from = page.glyphs.partition_point(|g| g.cy < miny);
            let to = page.glyphs.partition_point(|g| g.cy <= maxy);
            &page.glyphs[from..to.max(from)]
        };
        let cost = runs.len() + line.len();
        if cost > *work {
            *work = 0;
            return None;
        }
        *work -= cost;
        // Text on this line in a font we cannot measure: not guessing.
        if runs.iter().any(|&(x, _)| x <= maxx) {
            return None;
        }
        let mut hit: Vec<&Glyph> = line
            .iter()
            .filter(|g| g.cx >= minx && g.cx <= maxx)
            .collect();
        hit.sort_by_key(|g| g.seq);
        out.push(' ');
        out.extend(hit.iter().map(|g| g.ch));
        if out.len() > MAX_TEXT_BYTES * 2 {
            // Enough to fill the cap even after whitespace collapses.
            break;
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then(|| cap_text(&collapsed))
}

fn min_of(v: &[f64]) -> f64 {
    v.iter().copied().fold(f64::INFINITY, f64::min)
}
fn max_of(v: &[f64]) -> f64 {
    v.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

// ---- becoming highlights ------------------------------------------------------

fn num(n: f64) -> String {
    let s = format!("{:.2}", if n == 0.0 { 0.0 } else { n });
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// `p{page}:{tag}:{numbers}`, never longer than [`MAX_POS0_BYTES`]: numbers are
/// added a whole quad (or the whole rect) at a time, and a quad that would
/// cross the cap is left out with every one after it.
fn pos0_of(a: &PdfAnnotation) -> String {
    let groups: Vec<&[f64]> = if a.kind.is_markup() && !a.quads.is_empty() {
        a.quads.iter().map(|q| &q[..]).collect()
    } else {
        a.rect.as_ref().map(|r| vec![&r[..]]).unwrap_or_default()
    };
    let mut s = format!("p{}:{}:", a.page, a.kind.tag());
    let mut first = true;
    for g in groups {
        let part = g.iter().map(|n| num(*n)).collect::<Vec<_>>().join(",");
        if s.len() + 1 + part.len() > MAX_POS0_BYTES {
            break;
        }
        if !first {
            s.push(',');
        }
        s.push_str(&part);
        first = false;
    }
    s
}

/// The annotation with every non-finite coordinate gone: quads all-or-nothing,
/// as the scan does, and a non-finite `/Rect` dropped. A scan never yields one;
/// this is for a [`PdfScan`] built by hand, so a NaN or an infinity can never
/// reach `pos0` or the identity.
fn finite(a: &PdfAnnotation) -> PdfAnnotation {
    let mut a = a.clone();
    if !a.quads.iter().flatten().all(|n| n.is_finite()) {
        a.quads.clear();
    }
    a.rect = a.rect.filter(|r| r.iter().all(|n| n.is_finite()));
    a
}

/// The rounded box identity is taken from: `/Rect`, else the quads' bounds.
fn identity_box(a: &PdfAnnotation) -> [i64; 4] {
    let r = a.rect.unwrap_or_else(|| {
        let xs: Vec<f64> = a
            .quads
            .iter()
            .flat_map(|q| [q[0], q[2], q[4], q[6]])
            .collect();
        let ys: Vec<f64> = a
            .quads
            .iter()
            .flat_map(|q| [q[1], q[3], q[5], q[7]])
            .collect();
        if xs.is_empty() {
            [0.0; 4]
        } else {
            [min_of(&xs), min_of(&ys), max_of(&xs), max_of(&ys)]
        }
    });
    // Normalise the corner order (a /Rect may list either diagonal), then round.
    let (x0, x1) = (r[0].min(r[2]), r[0].max(r[2]));
    let (y0, y1) = (r[1].min(r[3]), r[1].max(r[3]));
    [x0, y0, x1, y1].map(|n| n.round() as i64)
}

/// Turn a scan into the highlights the shared path stores, in file order.
pub fn highlights_of(scan: &PdfScan, copy_id: Option<i64>) -> Vec<ImportedHighlight> {
    let mut names: HashSet<&str> = HashSet::new();
    let mut identities: HashMap<String, usize> = HashMap::new();
    // An /NM repeated in the file is not an identity; only its first holder uses it.
    let repeated: HashSet<&str> = {
        let mut seen = HashSet::new();
        scan.annotations
            .iter()
            .filter_map(|a| a.name.as_deref())
            .filter(|n| !seen.insert(*n))
            .collect()
    };
    scan.annotations
        .iter()
        .map(|a| {
            let place = finite(a);
            let b = identity_box(&place);
            let base = format!(
                "{}|{}|{},{},{},{}",
                a.page,
                a.kind.tag(),
                b[0],
                b[1],
                b[2],
                b[3]
            );
            let n = identities.entry(base.clone()).or_insert(0);
            *n += 1;
            let identity = if *n == 1 {
                base
            } else {
                format!("{base}|#{n}")
            };
            let origin_id = a
                .name
                .as_deref()
                .filter(|nm| !repeated.contains(nm) && names.insert(nm))
                .map(str::to_string);
            let (text, text_unavailable) = if a.kind.is_markup() {
                match &a.text {
                    Some(t) => (t.clone(), false),
                    None => (String::new(), true),
                }
            } else {
                (String::new(), false)
            };
            ImportedHighlight {
                origin: HighlightOrigin::Preview,
                copy_id,
                // `/NM` and the place are unique only inside this file.
                identity_scope: IdentityScope::Copy,
                origin_id,
                identity,
                text,
                text_unavailable,
                origin_note: a.contents.clone(),
                color: a.color.clone(),
                chapter: None,
                page: Some(i64::from(a.page)),
                pos_kind: Some(PosKind::PdfRect),
                pos0: Some(pos0_of(&place)),
                pos1: None,
                made_at: a.made_at.clone(),
            }
        })
        .collect()
}

/// Import one PDF's annotations as Preview highlights of `book_id`.
///
/// `copy_id` is the copy the file is a place of (stamped on new rows, and the
/// scope of the deletion rule). A file that cannot be read degrades to a
/// [`Diagnostic`] in the summary (`Ok`, nothing imported, nothing removed);
/// `Err` is only a storage failure.
pub async fn import_pdf_annotations(
    storage: &Storage,
    book_id: i64,
    copy_id: Option<i64>,
    path: &Path,
) -> Result<HighlightImportSummary> {
    let mut summary = HighlightImportSummary::default();
    let owned: PathBuf = path.to_path_buf();
    let scan = tokio::task::spawn_blocking(move || scan_pdf_annotations(&owned))
        .await
        .map_err(|e| EngineError::Other(format!("pdf scan task: {e}")))?;
    let scan = match scan {
        Ok(scan) => scan,
        Err(d) => {
            summary.diagnostics.push(d);
            return Ok(summary);
        }
    };
    summary.unparsed = scan.unparsed;
    let scan_complete = scan.is_complete();

    let mut seen: HashSet<i64> = HashSet::new();
    for h in highlights_of(&scan, copy_id) {
        let outcome = match storage.upsert_imported_highlight(book_id, &h).await {
            Ok(o) => o,
            // Every slot for this key is held by other copies' rows (storage
            // module doc, "Known limits"): it cannot be stored, which is what
            // `unparsed` counts.
            Err(EngineError::InvalidInput(_)) => {
                summary.unparsed += 1;
                continue;
            }
            Err(e) => return Err(e),
        };
        match outcome {
            ImportedOutcome::Inserted(id) => {
                summary.inserted += 1;
                seen.insert(id);
            }
            ImportedOutcome::Updated(id) => {
                summary.updated += 1;
                seen.insert(id);
            }
            ImportedOutcome::Unchanged(id) => {
                summary.unchanged += 1;
                seen.insert(id);
            }
        }
    }

    // Only a copy that is this one file can say which rows the file dropped.
    let removable = match copy_id {
        Some(copy) => storage.get_copy(copy).await?.is_some_and(|c| {
            c.places
                .iter()
                .filter(|p| p.kind == PlaceKind::Path)
                .count()
                <= 1
        }),
        None => false,
    };
    // And only a **complete** read of it can say so: after a scan cut short (a
    // cap reached, a page not all read, anything unparsed or unstorable), a
    // row this pass did not see may simply be one it did not read. Removing it
    // would delete the reader's highlight because the file was large.
    let complete = scan_complete && summary.unparsed == 0;
    if removable && !complete {
        summary
            .diagnostics
            .push(Diagnostic::pdf_scan_incomplete(path.to_path_buf()));
    }
    if let Some(copy) = copy_id.filter(|_| removable && complete) {
        let mut kept = 0usize;
        for row in storage
            .imported_highlights(book_id, HighlightOrigin::Preview)
            .await?
        {
            if row.copy_id != Some(copy) || seen.contains(&row.id) {
                continue;
            }
            if storage.imported_highlight_kept(row.id).await? {
                kept += 1;
            } else if storage.remove_imported_highlight_row(row.id).await? {
                summary.removed += 1;
            }
        }
        if kept > 0 {
            summary
                .diagnostics
                .push(Diagnostic::pdf_annotations_kept(path.to_path_buf(), kept));
        }
    }
    tracing::debug!(
        inserted = summary.inserted,
        updated = summary.updated,
        unchanged = summary.unchanged,
        removed = summary.removed,
        "imported pdf annotations"
    );
    Ok(summary)
}

// ---- the rescan rule ----------------------------------------------------------

/// What [`rescan_path_place`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RescanOutcome {
    /// The file's mtime is the one recorded: not read.
    Unchanged,
    /// The mtime moved: re-imported, and the place's hash and mtime refreshed.
    Reimported,
    /// The file is not there: the place is marked missing, the copy untouched.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceRescan {
    pub outcome: RescanOutcome,
    /// Empty unless `outcome` is `Reimported`.
    pub summary: HighlightImportSummary,
}

fn mtime_secs(meta: &std::fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// Rescan one `path` place of a copy: re-import its annotations only when the
/// file's mtime differs from the place's `last_mtime`.
///
/// Annotating a PDF in Preview rewrites it, so its hash changes while it is
/// still the same place and so the **same copy** (a `path` place is identified
/// by its path, not its bytes): the place's
/// `last_hash` and `last_mtime` are refreshed in step and no copy is made. A
/// file that is gone marks the place missing (a fact about the place, never a
/// verdict on the copy) and is not an error. A file that is back clears it.
pub async fn rescan_path_place(
    storage: &Storage,
    copy_id: i64,
    reference: &str,
) -> Result<PlaceRescan> {
    let copy = storage
        .get_copy(copy_id)
        .await?
        .ok_or_else(|| EngineError::NotFound(format!("copy id {copy_id}")))?;
    let place = copy
        .places
        .iter()
        .find(|p| p.kind == PlaceKind::Path && p.reference == reference)
        .ok_or_else(|| EngineError::NotFound(format!("path place of copy {copy_id}")))?;
    let path = Path::new(reference);
    let meta = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m,
        Ok(_) => {
            storage
                .mark_place_missing(PlaceKind::Path, reference, crate::storage::now_unix())
                .await?;
            return Ok(PlaceRescan {
                outcome: RescanOutcome::Missing,
                summary: HighlightImportSummary::default(),
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            storage
                .mark_place_missing(PlaceKind::Path, reference, crate::storage::now_unix())
                .await?;
            return Ok(PlaceRescan {
                outcome: RescanOutcome::Missing,
                summary: HighlightImportSummary::default(),
            });
        }
        Err(e) => return Err(e.into()),
    };
    storage.unmiss_place(PlaceKind::Path, reference).await?;
    let mtime = mtime_secs(&meta);
    if mtime.is_some() && place.last_mtime == mtime {
        // Looked at, found as it was: that is still a look. `last_seen_at`
        // moves (the Copies place says when Preview's highlights were last
        // read in); the hash and mtime stay what they were.
        storage
            .refresh_place(
                PlaceKind::Path,
                reference,
                place.last_hash.as_deref(),
                place.last_mtime,
            )
            .await?;
        return Ok(PlaceRescan {
            outcome: RescanOutcome::Unchanged,
            summary: HighlightImportSummary::default(),
        });
    }
    let summary = import_pdf_annotations(storage, copy.book_id, Some(copy_id), path).await?;
    let hash = crate::files::sha256_of(path)?;
    storage
        .refresh_place(PlaceKind::Path, reference, Some(&hash), mtime)
        .await?;
    Ok(PlaceRescan {
        outcome: RescanOutcome::Reimported,
        summary,
    })
}

// ---- fixtures -----------------------------------------------------------------

/// One page of a [`synthetic_annotated_pdf`].
#[cfg(any(test, feature = "internals"))]
#[derive(Debug, Clone, Default)]
pub struct SyntheticPage {
    /// `(x, y, font, text)`; font 1 is Courier (measurable), 2 is Helvetica with
    /// no `/Widths` (not measurable, so words under a highlight are unavailable).
    pub lines: Vec<(f64, f64, u8, String)>,
    /// Each annotation's dictionary body, e.g. `/Subtype /Highlight /Rect [..]`.
    pub annots: Vec<String>,
}

/// A classic PDF 1.4 file with the given pages, hand-assembled as bytes with an
/// exact cross-reference table, so no fixture depends on `lopdf`'s writer or
/// parser (the engine's parser must not build its own goldens). Courier 12 is
/// 7.2 points per character, which is what the tests' quad points assume.
///
/// Test-only, behind `internals`, like [`crate::pdf::synthetic_pdf`].
#[cfg(any(test, feature = "internals"))]
pub fn synthetic_annotated_pdf(pages: &[SyntheticPage]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let push = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, n: usize, body: &str| {
        assert_eq!(offsets.len() + 1, n, "objects are written in number order");
        offsets.push(out.len());
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    };
    // 1 catalog, 2 pages, 3 Courier, 4 Helvetica; then per page: page, content, annots.
    let mut bases = Vec::new();
    let mut next = 5;
    for p in pages {
        bases.push(next);
        next += 2 + p.annots.len();
    }
    push(
        &mut out,
        &mut offsets,
        1,
        "<< /Type /Catalog /Pages 2 0 R >>",
    );
    let kids: String = bases.iter().map(|b| format!("{b} 0 R ")).collect();
    push(
        &mut out,
        &mut offsets,
        2,
        &format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len()),
    );
    push(
        &mut out,
        &mut offsets,
        3,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>",
    );
    push(
        &mut out,
        &mut offsets,
        4,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    );
    for (p, &base) in pages.iter().zip(&bases) {
        let annots: String = (0..p.annots.len())
            .map(|i| format!("{} 0 R ", base + 2 + i))
            .collect();
        push(
            &mut out,
            &mut offsets,
            base,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> \
                 /Contents {} 0 R /Annots [{annots}] >>",
                base + 1
            ),
        );
        let mut stream = String::new();
        for (x, y, font, text) in &p.lines {
            let esc = text
                .replace('\\', "\\\\")
                .replace('(', "\\(")
                .replace(')', "\\)");
            stream.push_str(&format!("BT /F{font} 12 Tf {x} {y} Td ({esc}) Tj ET\n"));
        }
        push(
            &mut out,
            &mut offsets,
            base + 1,
            &format!("<< /Length {} >>\nstream\n{stream}endstream", stream.len()),
        );
        for (i, a) in p.annots.iter().enumerate() {
            push(
                &mut out,
                &mut offsets,
                base + 2 + i,
                &format!("<< /Type /Annot {a} >>"),
            );
        }
    }
    let count = offsets.len() + 1;
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
    for off in &offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `Unchanged` rescan is still a look: the place's `last_seen_at`
    /// moves (it is when Preview's highlights were last read in), while its
    /// hash and mtime stay what they were and nothing is imported.
    #[tokio::test]
    async fn an_unchanged_rescan_still_stamps_when_the_place_was_seen() {
        use crate::copies::{CopyForm, LearnedFrom, NewCopy, NewPlace, Tenure};
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let book = s
            .upsert_book(
                &crate::Book {
                    title: Some("A".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.pdf");
        std::fs::write(&p, synthetic_annotated_pdf(&[SyntheticPage::default()])).unwrap();
        let copy = s
            .create_copy(&NewCopy::new(
                book,
                Some(CopyForm::File),
                Tenure::Own,
                LearnedFrom::FileImport,
            ))
            .await
            .unwrap()
            .id;
        let reference = p.display().to_string();
        let mut place = NewPlace::new(PlaceKind::Path, reference.clone());
        place.last_hash = Some("recorded-hash".into());
        place.last_mtime = mtime_secs(&std::fs::metadata(&p).unwrap());
        s.attach_place(copy, &place).await.unwrap();
        sqlx::query("UPDATE copy_places SET last_seen_at = 1000 WHERE ref = ?")
            .bind(&reference)
            .execute(s.pool())
            .await
            .unwrap();

        let r = rescan_path_place(&s, copy, &reference).await.unwrap();
        assert_eq!(r.outcome, RescanOutcome::Unchanged);
        assert_eq!(r.summary, HighlightImportSummary::default());
        let after = &s.get_copy(copy).await.unwrap().unwrap().places[0];
        assert!(after.last_seen_at.unwrap() > 1000, "{after:?}");
        assert_eq!(after.last_hash.as_deref(), Some("recorded-hash"));
        assert_eq!(after.last_mtime, place.last_mtime);
    }

    #[test]
    fn dates_are_normalised_to_utc_or_kept_as_written() {
        assert_eq!(pdf_date("D:20260105211408+01'00'"), "2026-01-05 20:14:08");
        assert_eq!(pdf_date("D:20260105211408-05'30'"), "2026-01-06 02:44:08");
        assert_eq!(pdf_date("D:20260105211408Z"), "2026-01-05 21:14:08");
        assert_eq!(pdf_date("D:2026"), "2026-01-01 00:00:00");
        assert_eq!(pdf_date("last tuesday"), "last tuesday");
        assert_eq!(pdf_date("D:20261399"), "D:20261399");
    }

    #[test]
    fn colours_fold_to_rgb_hex() {
        assert_eq!(color_hex(&[1.0, 1.0, 0.0]).as_deref(), Some("#ffff00"));
        assert_eq!(color_hex(&[0.5]).as_deref(), Some("#808080"));
        assert_eq!(color_hex(&[0.0, 0.0, 1.0, 0.0]).as_deref(), Some("#ffff00"));
        assert_eq!(color_hex(&[]), None);
        assert_eq!(color_hex(&[1.0, 2.0]), None);
    }

    #[test]
    fn strings_decode_as_utf16_utf8_or_latin1() {
        assert_eq!(pdf_text(&[0xFE, 0xFF, 0x00, 0x68, 0x00, 0x69]), "hi");
        assert_eq!(pdf_text(&[0xEF, 0xBB, 0xBF, b'o', b'k']), "ok");
        assert_eq!(pdf_text(b"plain"), "plain");
    }

    /// Security review finding B. A simple font whose `/Encoding` is the
    /// *name* `Identity-H` has lopdf inflate its `/ToUnicode`; the first cut
    /// probed `/ToUnicode` only when `/Encoding` was absent, so that stream was
    /// decoded and never charged. Built as lopdf objects in memory rather than
    /// through `synthetic_annotated_pdf`, whose two fonts are fixed: this
    /// asserts a budget, not a golden, so nothing here is the parser grading
    /// its own output.
    #[test]
    fn every_stream_lopdf_inflates_for_an_encoding_is_charged() {
        use lopdf::{Dictionary, Document, Object, Stream};
        const N: usize = 10_000;
        let mut doc = Document::with_version("1.4");
        let to_unicode = doc.add_object(Stream::new(Dictionary::new(), vec![b' '; N]));
        let font = |encoding: &[u8]| {
            let mut f = Dictionary::new();
            f.set("Type", Object::Name(b"Font".to_vec()));
            f.set("Subtype", Object::Name(b"Type1".to_vec()));
            f.set("BaseFont", Object::Name(b"Courier".to_vec()));
            f.set("Encoding", Object::Name(encoding.to_vec()));
            f.set("ToUnicode", Object::Reference(to_unicode));
            f
        };

        // Identity-named: the probe and lopdf's own decode are both paid for.
        let identity = font(b"Identity-H");
        let mut budget = Budget::new();
        let _ = font_info(&doc, &identity, &mut budget);
        assert_eq!(MAX_FILE_FONT_BYTES - budget.font_bytes, 2 * N);

        // A named simple encoding: lopdf never touches `/ToUnicode`, so
        // nothing is inflated or charged for it.
        let mut budget = Budget::new();
        let _ = font_info(&doc, &font(b"WinAnsiEncoding"), &mut budget);
        assert_eq!(budget.font_bytes, MAX_FILE_FONT_BYTES);

        // Less left than the stream: refused before lopdf is asked.
        let mut budget = Budget::new();
        budget.font_bytes = N / 2;
        let info = font_info(&doc, &identity, &mut budget);
        assert!(info.encoding.is_none() && info.widths.is_none());
        assert_eq!(budget.font_bytes, 0);

        // Enough for the probe and not for lopdf's decode: refused too, and the
        // total spent never passes what the file had.
        let mut budget = Budget::new();
        budget.font_bytes = N + N / 2;
        let info = font_info(&doc, &identity, &mut budget);
        assert!(info.encoding.is_none());
        assert_eq!(budget.font_bytes, 0);
    }

    #[test]
    fn numbers_print_compactly() {
        assert_eq!(num(144.0), "144");
        assert_eq!(num(208.8), "208.8");
        assert_eq!(num(-0.0), "0");
    }
}
