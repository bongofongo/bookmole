//! Tier 2: sidecars derived from real Project Gutenberg epubs — the
//! `koreader-modern`, `koreader-legacy` and `koreader-stats` layers of the
//! testing corpus.
//!
//! Output must be a pure function of `(seed, GENERATOR_VERSION, epub bytes,
//! hostile.json)`. Everything here exists to keep that true: a pinned PRNG
//! algorithm, a fixed calendar epoch, sorted iteration, and a lock file
//! recording what produced what.
//!
//! # What a book's sidecar now carries (v3)
//!
//! * **One reader's calendar**, so the data is believable as one person's. Books are opened in manifest order,
//!   a few days apart, so one to three are open at once; each reading lasts
//!   5–40 days, with a holiday burst and two long gaps. Status is drawn
//!   `reading`/`complete`/`abandoned` at roughly 60/30/10.
//! * **Evening sessions.** Highlights are dated inside the reading window,
//!   clustered into a handful of evenings, in the order they occur in the book —
//!   and `stats/<layout>/koreader/settings/statistics.sql` records page turns on
//!   exactly those evenings, keyed by the sidecar's `partial_md5_checksum`,
//!   which is computed here by KOReader's rule over the real epub.
//! * **Real sentences.** A highlight is one to three whole sentences of one
//!   paragraph, cut at `.?!。！？` (plus closing quotes), never a character
//!   slice; 5–10% are a single real word, which the engine turns into
//!   flashcards.
//! * **Notes** on 30–40% of highlights, from `prose.rs`, in the book's language
//!   where the bank has one.
//! * **Salted hostile values** from `hostile.json` in `title`, `author`,
//!   `highlight`, `ko-note` and `lua-string` positions, at most one per row and
//!   well under 5% of rows, each tagged in the layout's `manifest.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use quick_xml::events::Event;
use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};
use sha2::{Digest, Sha256};

use crate::kostats::{self, CorpusStatsBook};
use crate::prose;

/// Bump deliberately when the *shape* of the output changes. That is the signal
/// to regenerate goldens; a seed change is not.
///
/// 2: the two layouts moved into `modern/` and `legacy/` subtrees. Emitted side
/// by side they were two sidecars claiming the *same* annotations of the same
/// book with different payloads, which no device can produce and which import
/// cannot converge on — see the comment at the write site.
///
/// 3: real reading state (status, percent, `doc_pages`, `summary`, `stats`,
/// `partial_md5_checksum`), sentence-cut highlights dated in evening sessions
/// of one calendar, prose-bank notes, hostile salting with per-layout
/// manifests, and the `stats/` statistics layer.
pub const GENERATOR_VERSION: u32 = 3;

/// Day zero of the reader's calendar: 2025-06-02, days since the unix epoch.
/// Never `now()` — a clock in the generator would churn every golden.
const CALENDAR_START: i64 = 20_241;

/// Characters per "page". crengine paginates from font and screen geometry,
/// which an epub does not determine; a fixed density is the honest stand-in,
/// and it keeps `doc_pages`, `pageno` and the statistics' `total_pages` in one
/// coordinate system.
const CHARS_PER_PAGE: usize = 1_800;

/// The hostile bank, compiled in so a generated corpus names the bank it was
/// salted from. The file is a hand-edited declaration; this module only
/// reads it.
const HOSTILE: &str = include_str!("../hostile.json");

/// One paragraph located within the book, in the coordinates crengine uses.
struct Para {
    /// 1-based index of the spine item. This is crengine's `DocFragment[N]`.
    fragment: usize,
    /// Element path within the fragment body, e.g. `p[12]` or `div[2]/p[3]`.
    element: String,
    text: String,
    /// Nearest preceding heading in the same fragment.
    chapter: Option<String>,
}

pub struct Options {
    pub seed: u64,
    pub per_book: usize,
}

/// One hostile case as declared.
struct Case {
    id: String,
    fields: Vec<String>,
    value: String,
    expect: String,
}

fn load_bank() -> Vec<Case> {
    let v: serde_json::Value =
        serde_json::from_str(HOSTILE).expect("hostile.json must be valid JSON");
    v["cases"]
        .as_array()
        .expect("hostile.json has a cases array")
        .iter()
        .map(|c| Case {
            id: c["id"].as_str().unwrap_or_default().to_string(),
            fields: c["fields"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|f| f.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            value: expand_template(c["value"].as_str().unwrap_or_default()),
            expect: c["expect"].as_str().unwrap_or("no-crash").to_string(),
        })
        .collect()
}

/// Expand the bank's `{repeat:s:n}` generator template. `{self_title}` belongs
/// to note bodies, which this layer does not write.
fn expand_template(v: &str) -> String {
    if let Some(inner) = v.strip_prefix("{repeat:").and_then(|r| r.strip_suffix('}'))
        && let Some((body, n)) = inner.rsplit_once(':')
        && let Ok(n) = n.parse::<usize>()
    {
        return body.repeat(n);
    }
    v.to_string()
}

/// Round-robin over the cases eligible for each field, so the few salt slots a
/// layer has spread across the bank instead of repeating one case.
struct Salter {
    by_field: BTreeMap<&'static str, Vec<usize>>,
    next: BTreeMap<&'static str, usize>,
}

/// The fields this layer salts. `lua-string` lands in a highlight's note
/// position, written *raw* for the escape-sequence case (the bank says the
/// parser is meant to decode it) and escaped otherwise.
const SALT_FIELDS: [&str; 5] = ["title", "author", "highlight", "ko-note", "lua-string"];

impl Salter {
    fn new(bank: &[Case]) -> Self {
        let mut by_field = BTreeMap::new();
        for f in SALT_FIELDS {
            let ids: Vec<usize> = bank
                .iter()
                .enumerate()
                .filter(|(_, c)| c.fields.iter().any(|x| x == f))
                .map(|(i, _)| i)
                .collect();
            by_field.insert(f, ids);
        }
        Salter {
            by_field,
            next: BTreeMap::new(),
        }
    }
    fn take(&mut self, field: &'static str) -> Option<usize> {
        let ids = self.by_field.get(field)?;
        if ids.is_empty() {
            return None;
        }
        let n = self.next.entry(field).or_insert(0);
        let i = ids[*n % ids.len()];
        *n += 1;
        Some(i)
    }
}

/// A salted row, for the layout manifest.
#[derive(Clone, serde::Serialize)]
struct Salt {
    sidecar: String,
    field: &'static str,
    case: String,
    expect: String,
    /// 1-based highlight index for row-level salts; absent for title/author.
    #[serde(skip_serializing_if = "Option::is_none")]
    row: Option<usize>,
    /// The value as salted, so a test can compare without re-expanding.
    value: String,
}

/// A highlight, fully decided before either layout encodes it.
struct Hl {
    fragment: usize,
    element: String,
    start: usize,
    end: usize,
    text: String,
    chapter: Option<String>,
    page: i64,
    /// `YYYY-MM-DD HH:MM:SS`, device-local (naive), inside a session.
    datetime: String,
    note: Option<String>,
    /// Write `note` into the Lua source without escaping (the
    /// `lua-escape-sequences` case).
    note_raw: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Reading,
    Complete,
    Abandoned,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Reading => "reading",
            Status::Complete => "complete",
            Status::Abandoned => "abandoned",
        }
    }
}

/// One book's place in the reader's calendar.
struct Plan {
    start_day: i64,
    end_day: i64,
    status: Status,
    /// 0.0..=1.0; exactly 1.0 when complete.
    percent: f64,
    rating: Option<i64>,
}

fn below(rng: &mut ChaCha8Rng, n: u32) -> u32 {
    rng.next_u32() % n.max(1)
}

/// Lay out the calendar over the books in manifest order. Its own PRNG stream,
/// so adding a book at the end of the manifest leaves every earlier plan alone.
fn calendar(n: usize, seed: u64) -> Vec<Plan> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed ^ 0xCA1E_00DA);
    let mut cursor = CALENDAR_START;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let start_day = cursor;
        let duration = 5 + below(&mut rng, 36) as i64;
        let r = below(&mut rng, 100);
        let status = if r < 60 {
            Status::Reading
        } else if r < 90 {
            Status::Complete
        } else {
            Status::Abandoned
        };
        let percent = match status {
            Status::Complete => 1.0,
            Status::Reading => 0.05 + below(&mut rng, 90) as f64 / 100.0,
            Status::Abandoned => 0.08 + below(&mut rng, 35) as f64 / 100.0,
        };
        // A reading still open is shorter on the calendar: it has not ended.
        let end_day = match status {
            Status::Complete => start_day + duration,
            _ => start_day + ((duration as f64 * percent.max(0.3)).round() as i64).max(2),
        };
        let rating = (status == Status::Complete && below(&mut rng, 10) < 6)
            .then(|| 3 + below(&mut rng, 3) as i64);
        out.push(Plan {
            start_day,
            end_day,
            status,
            percent,
            rating,
        });
        // One to three books open at once; a holiday burst (books 20–24 start a
        // day apart); two long gaps where the reader read nothing new.
        cursor += match i {
            20..=24 => 1,
            12 | 33 => 21 + below(&mut rng, 14) as i64,
            _ => 3 + below(&mut rng, 8) as i64,
        };
    }
    out
}

pub fn generate(
    manifest: &Path,
    epub_dir: &Path,
    out: &Path,
    opts: &Options,
) -> std::io::Result<usize> {
    let raw = std::fs::read_to_string(manifest)?;
    let man: serde_json::Value =
        serde_json::from_str(&raw).expect("corpus/manifest.json must be valid JSON");
    let books = man["books"].as_array().cloned().unwrap_or_default();

    let bank = load_bank();
    let mut salter = Salter::new(&bank);
    let mut salt_rng = ChaCha8Rng::seed_from_u64(opts.seed ^ 0x5A17_5A17);
    let plans = calendar(books.len(), opts.seed);

    std::fs::create_dir_all(out)?;
    // Stale v2 output must not survive into a v3 tree.
    for layout in ["modern", "legacy", "stats"] {
        let d = out.join(layout);
        if d.is_dir() {
            std::fs::remove_dir_all(&d)?;
        }
    }

    let mut lock = BTreeMap::new();
    let mut written = 0;
    let mut missing = Vec::new();
    let mut salts: Vec<Salt> = Vec::new();
    let mut sidecar_rows: Vec<serde_json::Value> = Vec::new();
    let mut stats_books: Vec<CorpusStatsBook> = Vec::new();
    let mut total_rows = 0usize;

    for (bi, b) in books.iter().enumerate() {
        let id = b["id"].as_u64().unwrap_or(0);
        let real_title = b["title"].as_str().unwrap_or("Untitled");
        let real_author = b["author"].as_str().unwrap_or("");
        let language = b["language"].as_str().unwrap_or("en");
        let epub_path = epub_dir.join(format!("{id}.epub"));
        let plan = &plans[bi];

        if !epub_path.is_file() {
            missing.push(id);
            continue;
        }
        let bytes = std::fs::read(&epub_path)?;
        let epub_sha = hex(&Sha256::digest(&bytes));
        let md5 = partial_md5(&bytes);

        // Project Gutenberg's own header and licence are not the book, and no
        // reader highlights them.
        // Some books (こころ) carry their whole text under a heading that names
        // Gutenberg, so the heading rule yields to the text rule when it would
        // leave nothing.
        let paras = match extract_paragraphs(&epub_path).map(|ps| {
            let text_ok: Vec<Para> = ps
                .into_iter()
                .filter(|p| !p.text.contains("Gutenberg"))
                .collect();
            let strict: Vec<usize> = text_ok
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    !p.chapter
                        .as_deref()
                        .is_some_and(|c| c.contains("Gutenberg"))
                })
                .map(|(i, _)| i)
                .collect();
            if strict.is_empty() {
                text_ok
            } else {
                text_ok
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| strict.binary_search(i).is_ok())
                    .map(|(_, p)| p)
                    .collect()
            }
        }) {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => {
                eprintln!("  {id}: no paragraphs found, skipping");
                continue;
            }
            Err(e) => {
                eprintln!("  {id}: unreadable ({e}), skipping");
                continue;
            }
        };

        // Seeded per book, so adding or removing a book cannot shift what every
        // other book generates.
        let mut rng = ChaCha8Rng::seed_from_u64(opts.seed ^ id.wrapping_mul(0x9E37_79B9));

        // Prefixed with the Gutenberg id, because a slug alone is neither
        // unique nor sufficient: every non-Latin title slugifies to nothing.
        let slug = format!("{id}-{}", slugify(real_title));

        // Title and author salts: at most one per sidecar, on fixed manifest
        // positions so the layer always carries both (a draw at this rate
        // over fifty books can come up empty).
        let mut title = real_title.to_string();
        let mut author = real_author.to_string();
        match bi % 25 {
            7 => {
                if let Some(ci) = salter.take("title") {
                    title = bank[ci].value.clone();
                    salts.push(salt(&slug, "title", &bank[ci], None));
                }
            }
            19 => {
                if let Some(ci) = salter.take("author") {
                    author = bank[ci].value.clone();
                    salts.push(salt(&slug, "author", &bank[ci], None));
                }
            }
            _ => {}
        }

        let doc_chars: usize = paras.iter().map(|p| p.text.chars().count()).sum();
        let doc_pages = doc_chars.div_ceil(CHARS_PER_PAGE).max(1) as i64;
        let cjk = matches!(language, "ja" | "zh");

        // ONE highlight set, encoded two ways. Drawing separately would give
        // the two layouts different passages, and the whole point of emitting
        // both is that they are the same content in two encodings — which makes
        // the corpus a differential test of the modern and legacy parsers.
        let target = ((opts.per_book as f64) * plan.percent.max(0.25))
            .round()
            .max(3.0) as usize;
        let mut hs = pick_highlights(&paras, target, plan.percent, cjk, &mut rng);
        page_and_date(&mut hs, &paras, plan, &mut rng);

        // Notes: 30–40% of highlights, from the language's bank.
        let bank_notes = prose::ko_notes(language);
        for h in hs.iter_mut() {
            if below(&mut rng, 100) < 35 {
                let t = bank_notes[below(&mut rng, bank_notes.len() as u32) as usize];
                h.note = Some(prose::note_for(t, &h.text, real_title, real_author));
            }
        }

        // Row salts: at most one per highlight, ~2% of rows.
        for (i, h) in hs.iter_mut().enumerate() {
            if below(&mut salt_rng, 50) != 0 {
                continue;
            }
            let field = ["highlight", "ko-note", "lua-string"][below(&mut salt_rng, 3) as usize];
            let Some(ci) = salter.take(field) else {
                continue;
            };
            let c = &bank[ci];
            match field {
                "highlight" => h.text = c.value.clone(),
                _ => {
                    h.note = Some(c.value.clone());
                    h.note_raw = field == "lua-string" && c.id == "lua-escape-sequences";
                }
            }
            salts.push(salt(&slug, field, c, Some(i + 1)));
        }
        total_rows += hs.len() + 1;

        let with_note = hs.iter().filter(|h| h.note.is_some()).count() as i64;
        let state = State {
            title: &title,
            author: &author,
            language,
            md5: &md5,
            doc_pages,
            plan,
            highlights: hs.len() as i64 - with_note,
            notes: with_note,
        };

        // **The two layouts go in separate trees, and that is not tidiness.**
        // They share `doc_props.title`, so side by side in one tree both match
        // the same library book; and they share `datetime`/`pos0`/`text`, so
        // their annotations share an `identity_hash`. But their payloads differ
        // in encoding, and import would refresh every row toward whichever
        // sidecar it read last, for ever. No device produces that, so the
        // corpus must not either.
        for (layout, body) in [
            ("modern", modern_sidecar(&hs, &state)),
            ("legacy", legacy_sidecar(&hs, &state)),
        ] {
            let dir = out.join(layout).join(format!("{slug}.sdr"));
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("metadata.epub.lua"), &body)?;
            lock.insert(
                format!("{layout}/{slug}.sdr/metadata.epub.lua"),
                hex(&Sha256::digest(body.as_bytes())),
            );
            written += 1;
        }
        lock.insert(format!("epub/{id}.epub"), epub_sha);

        sidecar_rows.push(serde_json::json!({
            "sidecar": slug,
            "gutenberg_id": id,
            "title": title,
            "authors": author,
            "language": language,
            "partial_md5": md5,
            "status": plan.status.as_str(),
            "percent_finished": plan.percent,
            "doc_pages": doc_pages,
            "rating": plan.rating,
            "reading_window": [kostats::ymd(plan.start_day), kostats::ymd(plan.end_day)],
            "highlights": hs.len(),
            "notes": with_note,
            "single_words": hs.iter().filter(|h| is_single_word(&h.text)).count(),
        }));

        stats_books.push(CorpusStatsBook {
            title: title.clone(),
            authors: author.clone(),
            language: language.to_string(),
            md5: md5.clone(),
            pages: doc_pages,
            highlights: hs.len() as i64 - with_note,
            notes: with_note,
            rows: stat_rows(&hs, &mut rng),
        });
    }

    assert!(
        salts.len() * 20 <= total_rows.max(1),
        "salting rule: at most 5% of rows ({} of {total_rows})",
        salts.len()
    );

    // Per-layout manifests: which sidecar carries which case, so a failure
    // names the case and the layer.
    for layout in ["modern", "legacy"] {
        let m = serde_json::json!({
            "layer": format!("koreader-{layout}"),
            "generator_version": GENERATOR_VERSION,
            "seed": opts.seed,
            "sidecars": sidecar_rows,
            "salted": salts,
        });
        let body = format!("{}\n", serde_json::to_string_pretty(&m).unwrap());
        std::fs::write(out.join(layout).join("manifest.json"), &body)?;
        lock.insert(
            format!("{layout}/manifest.json"),
            hex(&Sha256::digest(body.as_bytes())),
        );
    }

    // The statistics layer: one device install per layout, the same sessions.
    for layout in ["modern", "legacy"] {
        let root = out.join("stats").join(layout);
        let ko = root.join("koreader");
        std::fs::create_dir_all(ko.join("frontend"))?;
        std::fs::create_dir_all(ko.join("plugins"))?;
        std::fs::write(
            ko.join("reader.lua"),
            "-- generated KOReader install marker\n",
        )?;
        for p in kostats::write_corpus_stats(&ko.join("settings"), &stats_books)? {
            let body = std::fs::read(&p)?;
            let rel = p.strip_prefix(out).unwrap().to_string_lossy().into_owned();
            lock.insert(rel, hex(&Sha256::digest(&body)));
        }
    }

    // The lock is what lets a test tell "not generated yet" apart from
    // "generated, then hand-edited" — the second is a silent corruption of the
    // corpus that would otherwise pass unnoticed.
    let lockfile = serde_json::json!({
        "generator_version": GENERATOR_VERSION,
        "seed": opts.seed,
        "per_book": opts.per_book,
        "files": lock,
    });
    std::fs::write(
        out.join("corpus.lock.json"),
        format!("{}\n", serde_json::to_string_pretty(&lockfile).unwrap()),
    )?;

    if !missing.is_empty() {
        eprintln!(
            "note: {} epub(s) absent — run scripts/fetch-corpus.sh: {:?}",
            missing.len(),
            missing
        );
    }
    Ok(written)
}

fn salt(sidecar: &str, field: &'static str, c: &Case, row: Option<usize>) -> Salt {
    Salt {
        sidecar: sidecar.to_string(),
        field,
        case: c.id.clone(),
        expect: c.expect.clone(),
        row,
        value: c.value.clone(),
    }
}

/// The book-level facts both layouts write.
struct State<'a> {
    title: &'a str,
    author: &'a str,
    language: &'a str,
    md5: &'a str,
    doc_pages: i64,
    plan: &'a Plan,
    highlights: i64,
    notes: i64,
}

// ---- sentences --------------------------------------------------------------

const TERMINATORS: [char; 6] = ['.', '?', '!', '。', '！', '？'];
const CLOSERS: [char; 8] = ['"', '\'', '”', '’', '»', '」', '』', '›'];

/// Sentence spans `(start, end)` in **chars** within `text`, each ending in a
/// terminator or a closing quote after one. A trailing fragment with no
/// terminator is not a sentence and is not returned.
fn sentences(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if TERMINATORS.contains(&c) {
            let mut end = i + 1;
            while end < chars.len()
                && (TERMINATORS.contains(&chars[end]) || CLOSERS.contains(&chars[end]))
            {
                end += 1;
            }
            let cjk_stop = matches!(c, '。' | '！' | '？');
            let at_break = end >= chars.len() || chars[end].is_whitespace() || cjk_stop;
            if at_break && !(c == '.' && is_abbreviation(&chars[start..i])) {
                // Trim leading whitespace and stray closers left from the last cut.
                let mut s = start;
                while s < end
                    && (chars[s].is_whitespace()
                        || CLOSERS.contains(&chars[s])
                            && s == start
                            && chars[s] != '"'
                            && chars[s] != '\'')
                {
                    s += 1;
                }
                if end > s + 1 {
                    out.push((s, end));
                }
                start = end;
                i = end;
                continue;
            }
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

/// `Mr.`, `Mrs.`, `St.`, `M.` — a short capitalised word before a full stop
/// is almost always an abbreviation in this corpus, not a sentence end.
fn is_abbreviation(before: &[char]) -> bool {
    let word: String = before
        .iter()
        .rev()
        .take_while(|c| !c.is_whitespace())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let n = word.chars().count();
    (1..=3).contains(&n) && word.chars().next().is_some_and(char::is_uppercase)
}

/// A highlight that is one alphabetic run — the only shape `one_word` makes.
fn is_single_word(text: &str) -> bool {
    !text.is_empty() && text.chars().all(char::is_alphabetic)
}

/// Whether a terminator-bounded span reads as a sentence a person would mark:
/// in a spaced script, two or more words that do not open mid-sentence (so not
/// `IAGO.`, `767.` or `said the others.`); in a space-less one, four letters.
fn is_real_sentence(text: &str, cjk: bool) -> bool {
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    if cjk {
        return letters >= 4;
    }
    let words = text
        .split_whitespace()
        .filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 1)
        .count();
    let opens_lower = text
        .chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(char::is_lowercase);
    words >= 2 && letters >= 6 && !opens_lower
}

/// One real word of the paragraph, as `(start, end)` chars: alphabetic, four
/// or more letters. For a space-less script, two or three consecutive letters.
fn one_word(text: &str, cjk: bool, rng: &mut ChaCha8Rng) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut cands = Vec::new();
    if cjk {
        let mut i = 0;
        while i + 3 <= chars.len() {
            if chars[i..i + 3].iter().all(|c| c.is_alphabetic()) {
                cands.push((i, i + 2 + (i % 2)));
            }
            i += 3;
        }
    } else {
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_alphabetic() && (i == 0 || !chars[i - 1].is_alphanumeric()) {
                let mut j = i;
                while j < chars.len() && chars[j].is_alphabetic() {
                    j += 1;
                }
                let next_ok = j >= chars.len()
                    || !chars[j].is_alphanumeric() && chars[j] != '\'' && chars[j] != '’';
                if j - i >= 4 && next_ok {
                    cands.push((i, j));
                }
                i = j;
            } else {
                i += 1;
            }
        }
    }
    if cands.is_empty() {
        return None;
    }
    Some(cands[below(rng, cands.len() as u32) as usize])
}

/// Choose `n` highlights from the first `percent` of the book, sorted by
/// position, each 1–3 whole sentences of one paragraph or a single word.
fn pick_highlights(
    paras: &[Para],
    n: usize,
    percent: f64,
    cjk: bool,
    rng: &mut ChaCha8Rng,
) -> Vec<Hl> {
    let reach = ((paras.len() as f64 * percent).ceil() as usize).clamp(1, paras.len());
    let mut picked: BTreeMap<(usize, usize), Hl> = BTreeMap::new();
    let mut attempts = 0;
    while picked.len() < n && attempts < n * 20 {
        attempts += 1;
        let pi = below(rng, reach as u32) as usize;
        let p = &paras[pi];
        let chars: Vec<char> = p.text.chars().collect();
        // 5% of draws: a word. Sentence draws are rejected more often (labels,
        // numbers, mid-sentence cuts), so the realised share is ~7% (seed 42: 85/1270).
        let span = if below(rng, 100) < 5 {
            one_word(&p.text, cjk, rng)
        } else {
            let ss = sentences(&p.text);
            if ss.is_empty() {
                None
            } else {
                let first = below(rng, ss.len() as u32) as usize;
                let count = 1 + below(rng, 3) as usize;
                let last = (first + count - 1).min(ss.len() - 1);
                let (s, e) = (ss[first].0, ss[last].1);
                // A one-sentence paragraph of 2,000 characters is not a highlight,
                // and a speaker label or a list number is not a sentence.
                let text: String = chars[s..e].iter().collect();
                if e - s > 600 || !is_real_sentence(&text, cjk) {
                    None
                } else {
                    Some((s, e))
                }
            }
        };
        let Some((s, e)) = span else { continue };
        if picked.contains_key(&(pi, s)) {
            continue;
        }
        let text: String = chars[s..e].iter().collect();
        picked.insert(
            (pi, s),
            Hl {
                fragment: p.fragment,
                element: p.element.clone(),
                start: s,
                end: e,
                text,
                chapter: p.chapter.clone(),
                page: pi as i64, // paragraph index for now; `page_and_date` maps it
                datetime: String::new(),
                note: None,
                note_raw: false,
            },
        );
    }
    picked.into_values().collect()
}

/// Seconds since the unix epoch of a session's first page turn, per highlight;
/// plus the page each lands on. Highlights are in book order, so the sessions
/// move forward through the book as the evenings pass.
fn page_and_date(hs: &mut [Hl], paras: &[Para], plan: &Plan, rng: &mut ChaCha8Rng) {
    // Cumulative characters before each paragraph → page.
    let mut cum = Vec::with_capacity(paras.len());
    let mut acc = 0usize;
    for p in paras {
        cum.push(acc);
        acc += p.text.chars().count();
    }
    for h in hs.iter_mut() {
        let pi = h.page as usize;
        h.page = 1 + (cum[pi] + h.start) as i64 / CHARS_PER_PAGE as i64;
    }
    if hs.is_empty() {
        return;
    }
    // Session days: the first and (when complete) the last day of the window,
    // and a few evenings between.
    let span = (plan.end_day - plan.start_day).max(1);
    let want = (hs.len() / 4).clamp(1, 12).min(span as usize + 1);
    let mut days = std::collections::BTreeSet::new();
    days.insert(plan.start_day);
    if plan.status == Status::Complete {
        days.insert(plan.end_day);
    }
    let mut guard = 0;
    while days.len() < want && guard < 200 {
        days.insert(plan.start_day + below(rng, span as u32 + 1) as i64);
        guard += 1;
    }
    let days: Vec<i64> = days.into_iter().collect();
    let per = hs.len().div_ceil(days.len());
    for (si, chunk) in hs.chunks_mut(per).enumerate() {
        let day = days[si.min(days.len() - 1)];
        // An evening: 19:00–21:30 start, highlights four to nine minutes apart.
        let mut t = day * 86_400 + 19 * 3_600 + below(rng, 150) as i64 * 60 + 300;
        for h in chunk.iter_mut() {
            t += 240 + below(rng, 300) as i64;
            h.datetime = datetime(t);
        }
    }
}

/// `page_stat_data` rows for the sessions that produced `hs`: on each session
/// evening, every page from just before the first highlight to the last one,
/// in order, with times that bracket the highlights' own.
fn stat_rows(hs: &[Hl], rng: &mut ChaCha8Rng) -> Vec<(i64, i64, i64)> {
    let mut by_day: BTreeMap<i64, Vec<(i64, i64)>> = BTreeMap::new();
    for h in hs {
        let t = parse_datetime(&h.datetime);
        by_day
            .entry(t.div_euclid(86_400))
            .or_default()
            .push((t, h.page));
    }
    let mut rows = Vec::new();
    for (_, list) in by_day {
        let first_t = list.iter().map(|x| x.0).min().unwrap();
        let last_t = list.iter().map(|x| x.0).max().unwrap();
        let lo = list.iter().map(|x| x.1).min().unwrap();
        let hi = list.iter().map(|x| x.1).max().unwrap();
        let lo = (lo - 1 - below(rng, 3) as i64).max(1).max(hi - 39);
        let pages: Vec<i64> = (lo..=hi).collect();
        // Start a little before the first highlight; spread the pages across
        // the session so the last turn lands after the last highlight.
        let start = first_t - 120;
        let window = (last_t + 180 - start).max(pages.len() as i64 * 30);
        let step = window / pages.len() as i64;
        for (k, p) in pages.iter().enumerate() {
            let dur = (step - below(rng, 15) as i64).max(10);
            rows.push((*p, start + k as i64 * step, dur));
        }
    }
    rows
}

fn datetime(t: i64) -> String {
    let day = t.div_euclid(86_400);
    let s = t.rem_euclid(86_400);
    format!(
        "{} {:02}:{:02}:{:02}",
        kostats::ymd(day),
        s / 3_600,
        (s / 60) % 60,
        s % 60
    )
}

fn parse_datetime(s: &str) -> i64 {
    let (d, t) = s.split_once(' ').unwrap();
    let mut it = d.split('-').map(|x| x.parse::<i64>().unwrap());
    let (y, m, dd) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
    let mut tt = t.split(':').map(|x| x.parse::<i64>().unwrap());
    let (h, mi, se) = (tt.next().unwrap(), tt.next().unwrap(), tt.next().unwrap());
    days_from_civil(y, m, dd) * 86_400 + h * 3_600 + mi * 60 + se
}

/// Hinnant's days-from-civil; the inverse of `kostats::ymd`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// ---- sidecars ---------------------------------------------------------------

fn note_lua(h: &Hl) -> Option<String> {
    h.note
        .as_ref()
        .map(|n| if h.note_raw { n.clone() } else { lua_escape(n) })
}

/// The root keys every DocSettings file carries beside its annotations:
/// `doc_pages`, `percent_finished`, `partial_md5_checksum`, `summary`, `stats`
/// and `doc_props`.
fn root_keys(st: &State) -> String {
    let percent = if st.plan.status == Status::Complete {
        // A finished book is written as the bare integer `1` (koreader.rs).
        "1".to_string()
    } else {
        format!("{:.4}", st.plan.percent)
    };
    let rating = st
        .plan
        .rating
        .map(|r| format!("        [\"rating\"] = {r},\n"))
        .unwrap_or_default();
    format!(
        "    [\"doc_pages\"] = {pages},\n\
         \x20   [\"doc_props\"] = {{\n\
         \x20       [\"authors\"] = \"{author}\",\n\
         \x20       [\"language\"] = \"{lang}\",\n\
         \x20       [\"title\"] = \"{title}\",\n\
         \x20   }},\n\
         \x20   [\"partial_md5_checksum\"] = \"{md5}\",\n\
         \x20   [\"percent_finished\"] = {percent},\n\
         \x20   [\"stats\"] = {{\n\
         \x20       [\"authors\"] = \"{author}\",\n\
         \x20       [\"highlights\"] = {hl},\n\
         \x20       [\"language\"] = \"{lang}\",\n\
         \x20       [\"notes\"] = {notes},\n\
         \x20       [\"pages\"] = {pages},\n\
         \x20       [\"title\"] = \"{title}\",\n\
         \x20   }},\n\
         \x20   [\"summary\"] = {{\n\
         \x20       [\"modified\"] = \"{modified}\",\n\
         {rating}\
         \x20       [\"status\"] = \"{status}\",\n\
         \x20   }},\n",
        pages = st.doc_pages,
        author = lua_escape(st.author),
        lang = lua_escape(st.language),
        title = lua_escape(st.title),
        md5 = st.md5,
        hl = st.highlights,
        notes = st.notes,
        modified = kostats::ymd(st.plan.end_day),
        status = st.plan.status.as_str(),
    )
}

fn modern_sidecar(hs: &[Hl], st: &State) -> String {
    let mut s = String::from(
        "-- GENERATED from a Project Gutenberg epub by `corpus gen-corpus`.\n\
         -- Modern `annotations` layout. Do not hand-edit.\n\
         return {\n    [\"annotations\"] = {\n",
    );
    for (i, h) in hs.iter().enumerate() {
        let chapter_line = h
            .chapter
            .as_ref()
            .map(|c| format!("            [\"chapter\"] = \"{}\",\n", lua_escape(c)))
            .unwrap_or_default();
        let note_line = note_lua(h)
            .map(|n| format!("            [\"note\"] = \"{n}\",\n"))
            .unwrap_or_default();
        s.push_str(&format!(
            "        [{n}] = {{\n\
             {chapter_line}\
             \x20           [\"datetime\"] = \"{dt}\",\n\
             {note_line}\
             \x20           [\"pageno\"] = {page},\n\
             \x20           [\"pos0\"] = \"/body/DocFragment[{frag}]/body/{elem}/text().{start}\",\n\
             \x20           [\"pos1\"] = \"/body/DocFragment[{frag}]/body/{elem}/text().{end}\",\n\
             \x20           [\"text\"] = \"{text}\",\n\
             \x20       }},\n",
            n = i + 1,
            dt = h.datetime,
            page = h.page,
            frag = h.fragment,
            elem = h.element,
            start = h.start,
            end = h.end,
            text = lua_escape(&h.text),
        ));
    }
    s.push_str("    },\n");
    s.push_str(&root_keys(st));
    s.push_str("}\n");
    s
}

fn legacy_sidecar(hs: &[Hl], st: &State) -> String {
    let mut s = String::from(
        "-- GENERATED from a Project Gutenberg epub by `corpus gen-corpus`.\n\
         -- Legacy `highlight` + `bookmarks` layout. Do not hand-edit.\n\
         return {\n    [\"highlight\"] = {\n",
    );
    // Page-keyed, with every highlight on one page in that page's array.
    let mut by_page: BTreeMap<i64, Vec<&Hl>> = BTreeMap::new();
    for h in hs {
        by_page.entry(h.page).or_default().push(h);
    }
    for (page, list) in &by_page {
        s.push_str(&format!("        [{page}] = {{\n"));
        for (k, h) in list.iter().enumerate() {
            let chapter_line = h
                .chapter
                .as_ref()
                .map(|c| format!("                [\"chapter\"] = \"{}\",\n", lua_escape(c)))
                .unwrap_or_default();
            s.push_str(&format!(
                "            [{k}] = {{\n\
                 {chapter_line}\
                 \x20               [\"datetime\"] = \"{dt}\",\n\
                 \x20               [\"pos0\"] = \"/body/DocFragment[{frag}]/body/{elem}/text().{start}\",\n\
                 \x20               [\"pos1\"] = \"/body/DocFragment[{frag}]/body/{elem}/text().{end}\",\n\
                 \x20               [\"text\"] = \"{text}\",\n\
                 \x20           }},\n",
                k = k + 1,
                dt = h.datetime,
                frag = h.fragment,
                elem = h.element,
                start = h.start,
                end = h.end,
                text = lua_escape(&h.text),
            ));
        }
        s.push_str("        },\n");
    }
    s.push_str("    },\n    [\"bookmarks\"] = {\n");
    // Every highlight has a bookmark; a note is the bookmark's `text` where it
    // differs from `notes` (the highlighted text) — joined by datetime, the
    // legacy format's one genuinely awkward feature.
    for (i, h) in hs.iter().enumerate() {
        let text = note_lua(h).unwrap_or_else(|| lua_escape(&h.text));
        s.push_str(&format!(
            "        [{n}] = {{\n\
             \x20           [\"datetime\"] = \"{dt}\",\n\
             \x20           [\"notes\"] = \"{notes}\",\n\
             \x20           [\"page\"] = {page},\n\
             \x20           [\"text\"] = \"{text}\",\n\
             \x20       }},\n",
            n = i + 1,
            dt = h.datetime,
            notes = lua_escape(&h.text),
            page = h.page,
        ));
    }
    s.push_str("    },\n");
    s.push_str(&root_keys(st));
    s.push_str("}\n");
    s
}

/// Escape for a Lua double-quoted string. The corpus is full of quotation
/// marks, backslashes and newlines straight out of nineteenth-century prose —
/// and, since v3, salted control characters, which are written as decimal
/// escapes so the file stays text and the parser decodes the exact bytes.
fn lua_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\{:03}", c as u32))
            }
            c => out.push(c),
        }
    }
    out
}

// ---- KOReader's partialMD5 --------------------------------------------------

/// KOReader's `util.partialMD5`: MD5 over twelve 1 KiB
/// windows at 0, 1 Ki, 4 Ki, … 1 Gi, stopping at the first empty read. A short
/// read at EOF is still hashed. Reimplemented here — the generator never calls
/// the engine, or the engine's bug would become the fixture's truth.
fn partial_md5(bytes: &[u8]) -> String {
    let mut buf = Vec::new();
    for i in -1i32..=10 {
        let offset: usize = if i < 0 {
            0
        } else {
            1024usize << (2 * i as u32)
        };
        if offset >= bytes.len() {
            break;
        }
        let end = (offset + 1024).min(bytes.len());
        buf.extend_from_slice(&bytes[offset..end]);
    }
    hex(&md5(&buf))
}

/// RFC 1321 MD5. Written out (sixty lines) rather than adding a dependency to
/// the generator; checked against the RFC's own test vectors below.
fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());
    let (mut a0, mut b0, mut c0, mut d0) = (
        0x6745_2301u32,
        0xefcd_ab89u32,
        0x98ba_dcfeu32,
        0x1032_5476u32,
    );
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f2 = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f2.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, v) in [a0, b0, c0, d0].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

// ---- extraction ---------------------------------------------------------------

/// Walk the spine in order and collect every paragraph, with the crengine
/// coordinates a KOReader xpointer is built from.
///
/// The mapping is mechanical: `DocFragment[N]` is the N-th spine item (1-based,
/// in OPF spine order), and within it `p[M]` / `div[k]/p[M]` addresses the
/// element.
fn extract_paragraphs(path: &Path) -> Result<Vec<Para>, String> {
    let mut doc = crate::epub_read::Epub::open(path)?;
    // The idref is the manifest key we resolve.
    let spine = doc.spine().to_vec();
    let mut out = Vec::new();

    for (idx, item) in spine.iter().enumerate() {
        let Some(content) = doc.resource(item) else {
            continue;
        };
        let xhtml = String::from_utf8_lossy(&content).into_owned();
        out.extend(paragraphs_of(&xhtml, idx + 1));
    }
    Ok(out)
}

/// Pull-parse one XHTML fragment. Deliberately simple: epub content is
/// well-formed XML, and only `<p>`, `<div>` and `<h1..h3>` matter here.
fn paragraphs_of(xhtml: &str, fragment: usize) -> Vec<Para> {
    let mut reader = quick_xml::Reader::from_str(xhtml);
    reader.config_mut().trim_text(true);

    let mut out = Vec::new();
    let mut buf = Vec::new();
    let mut p_index = 0usize;
    let mut div_index = 0usize;
    let mut in_div = false;
    let mut chapter: Option<String> = None;

    let mut capturing: Option<&'static str> = None;
    let mut current = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_lowercase();
                match name.as_str() {
                    "p" => {
                        p_index += 1;
                        capturing = Some("p");
                        current.clear();
                    }
                    "div" | "section" => {
                        div_index += 1;
                        in_div = true;
                    }
                    "h1" | "h2" | "h3" => {
                        capturing = Some("h");
                        current.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if capturing.is_some() {
                    current.push_str(&t.xml10_content().unwrap_or_default());
                    current.push(' ');
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_lowercase();
                match name.as_str() {
                    "p" => {
                        let text = squash(&current);
                        // Very short paragraphs make poor highlights.
                        if text.chars().count() >= 40 {
                            let element = if in_div {
                                format!("div[{div_index}]/p[{p_index}]")
                            } else {
                                format!("p[{p_index}]")
                            };
                            out.push(Para {
                                fragment,
                                element,
                                text,
                                chapter: chapter.clone(),
                            });
                        }
                        capturing = None;
                    }
                    "h1" | "h2" | "h3" => {
                        let t = squash(&current);
                        if !t.is_empty() {
                            chapter = Some(t);
                        }
                        capturing = None;
                    }
                    "div" | "section" => in_div = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    let t = out.trim_matches('-').to_string();
    if t.is_empty() { "untitled".into() } else { t }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn default_epub_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/epub")
}

pub fn default_manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/manifest.json")
}

pub fn default_out() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/generated")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_the_rfc_vectors() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&md5(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            )),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }

    #[test]
    fn partial_md5_of_a_small_file_is_the_md5_of_its_first_kib() {
        let small = vec![7u8; 900];
        assert_eq!(partial_md5(&small), hex(&md5(&small)));
        // 5 KiB: windows at 0 and 1 Ki and 4 Ki (a short one), then 16 Ki is past EOF.
        let big: Vec<u8> = (0..5 * 1024).map(|i| (i % 251) as u8).collect();
        let mut want = big[0..1024].to_vec();
        want.extend_from_slice(&big[1024..2048]);
        want.extend_from_slice(&big[4096..5120]);
        assert_eq!(partial_md5(&big), hex(&md5(&want)));
    }

    #[test]
    fn labels_numbers_and_mid_sentence_cuts_are_not_sentences() {
        for bad in [
            "IAGO.",
            "MEPHISTOPHELES.",
            "767.",
            "1.",
            "said the others.",
            "Ah!",
        ] {
            assert!(!is_real_sentence(bad, false), "{bad:?}");
        }
        for good in [
            "It is a truth universally acknowledged.",
            "Ἰοὺ ἰού, ὢ ὢ κακά.",
        ] {
            assert!(is_real_sentence(good, false), "{good:?}");
        }
        assert!(is_real_sentence("道可道，非常道。", true));
        assert!(!is_real_sentence("一。", true));
        assert!(is_single_word("Darcy") && is_single_word("道可"));
        assert!(!is_single_word("IAGO.") && !is_single_word("22."));
    }

    #[test]
    fn sentences_end_in_terminal_punctuation() {
        let t = "Mr. Darcy bowed. \"Is it so?\" she asked! And then a fragment";
        let got: Vec<String> = sentences(t)
            .into_iter()
            .map(|(s, e)| t.chars().skip(s).take(e - s).collect())
            .collect();
        assert_eq!(got, vec!["Mr. Darcy bowed.", "\"Is it so?\"", "she asked!"]);
        let cjk = "吾十有五而志于学。三十而立！";
        assert_eq!(sentences(cjk).len(), 2);
    }

    #[test]
    fn the_calendar_is_roughly_sixty_thirty_ten() {
        let plans = calendar(400, 42);
        let n = |s: Status| plans.iter().filter(|p| p.status == s).count();
        assert!(
            (200..=280).contains(&n(Status::Reading)),
            "{}",
            n(Status::Reading)
        );
        assert!((90..=150).contains(&n(Status::Complete)));
        assert!((20..=65).contains(&n(Status::Abandoned)));
        for p in &plans {
            assert!(p.end_day > p.start_day);
        }
    }

    #[test]
    fn datetimes_round_trip() {
        for t in [0, 1_750_000_000, 1_767_225_599] {
            assert_eq!(parse_datetime(&datetime(t)), t);
        }
    }

    #[test]
    fn lua_escape_writes_controls_as_decimal_escapes() {
        assert_eq!(lua_escape("a\u{0}b\u{7}\r"), "a\\000b\\007\\r");
    }

    #[test]
    fn the_hostile_bank_loads_and_repeat_templates_expand() {
        let bank = load_bank();
        assert!(
            bank.iter()
                .any(|c| c.fields.iter().any(|f| f == "highlight"))
        );
        assert_eq!(expand_template("{repeat:ab:3}"), "ababab");
    }
}
