//! Layer `calibre`: a real calibre library, built by `calibredb` from the
//! Gutenberg epubs, plus a shelf of formatless ("physical") rows.
//!
//! One tier-2 layer: one avenue (calibre) at one realism level, generated
//! into its own directory and pinned by its own lock, with hostile values
//! salted in at a declared, low proportion.
//!
//! **The warts are the tool's.** Every row is created with `calibredb add` and
//! dressed with `calibredb set_metadata`, so `&`-joined authors, calibre's
//! `0101-01-01` undated sentinel, `series_index = 1.0` on a series-less row,
//! comma-split tags, the `isbn` column derived from the `identifiers` map and
//! the formats `ebook-convert` really produced are what calibre wrote, never
//! what this file thinks calibre writes. Recording them by hand would prove we
//! agree with ourselves.
//!
//! **Semantic lock, not byte lock.** calibre mints uuids,
//! timestamps and `last_modified` no seed controls, and it renders `pubdate`
//! through the local timezone (a leap-day pubdate of `2024-02-29` came back as
//! `2024-02-28T00:00:00+00:00` on a UTC+ machine — measured). So
//! `calibre.lock.json` records only what is a function of
//! `(seed, GENERATOR_VERSION, manifest)`: per row the calibre id (rows are
//! added one at a time, in plan order), title, authors as calibredb reports
//! them, format extensions, tags, series, series index, cover presence, isbn,
//! identifier keys and the publication *year*. The `metadata.db` bytes are not
//! deterministic and nothing asserts on them.
//!
//! **Safety.** Every invocation passes `--with-library` and points
//! `CALIBRE_CONFIG_DIRECTORY` at a private directory under the output, so the
//! user's own calibre settings and library are never read or written. The
//! generator refuses to touch a target whose `metadata.db` it did not create
//! (no marker file) and any path containing `Calibre Library`.
//!
//! **Salting**, the rule every layer follows: at most one hostile value per row, at most 5% of
//! rows, drawn from `crates/corpus/hostile.json` for the fields `title`,
//! `author`, `tag`, `isbn`, `date`. A value calibredb itself refuses (an
//! impossible date) is recorded as `calibre_rejected` and the row keeps its
//! benign metadata — the tool's refusal is the observation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};
use serde_json::{Value, json};

/// Bumped when the *shape* of the layer changes (which rows, which fields).
pub const GENERATOR_VERSION: u32 = 2;

/// Marker written into a library this generator created. Its absence beside a
/// `metadata.db` means somebody else's library, and the generator refuses.
const MARKER: &str = ".bookmole-corpus";

/// How many formatless rows join the Gutenberg ones. Calibre users track
/// physical books exactly this way (`calibredb add --empty`), and it is what
/// makes 5% of the rows enough to salt one case per field.
const PHYSICAL_ROWS: usize = 70;

/// Fields this layer salts, in the order cases are chosen.
const SALT_FIELDS: [&str; 5] = ["title", "author", "tag", "isbn", "date"];

/// Gutenberg ids that get a second format through `ebook-convert`. Short books
/// on purpose: a pdf of War and Peace costs minutes for no extra coverage.
const CONVERSIONS: [(u64, &str); 3] = [(1080, "pdf"), (11, "mobi"), (219, "azw3")];

/// Gutenberg ids whose epub cover is replaced by a rendered PNG.
const RENDERED_COVERS: [u64; 3] = [1952, 1404, 16328];

pub struct Options {
    pub seed: u64,
}

/// `corpus/generated/calibre`, gitignored like every other generated layer.
pub fn default_out() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("corpus/generated/calibre")
}

fn hostile_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("hostile.json")
}

// ---- banks -------------------------------------------------------------------

const ADJECTIVES: &[&str] = &[
    "Quiet", "Northern", "Lost", "Salt", "Winter", "Borrowed", "Paper", "Iron", "Hidden", "Last",
    "Small", "Drowned", "Glass", "Patient", "Silver", "Distant",
];
const NOUNS: &[&str] = &[
    "Orchard",
    "Harbour",
    "Lantern",
    "Archive",
    "Garden",
    "Ledger",
    "Lighthouse",
    "River",
    "Atlas",
    "Kitchen",
    "Clockmaker",
    "Island",
    "Library",
    "Weather",
    "Bridge",
    "Inheritance",
];
const PLACES: &[&str] = &[
    "Lisbon",
    "the Fens",
    "Kyoto",
    "Trieste",
    "Patagonia",
    "the Hebrides",
    "Odessa",
    "Lagos",
];
const FIRST: &[&str] = &[
    "Ada", "Tomas", "Ines", "Rafael", "Margit", "Kwame", "Helen", "Yusuf", "Clara", "Pieter",
    "Noor", "Elena", "Samuel", "Ingrid", "Mateo", "Rosalind",
];
const LAST: &[&str] = &[
    "Whitlock",
    "Okafor",
    "Lindqvist",
    "Moreau",
    "Castellanos",
    "Abernathy",
    "Novak",
    "Hale",
    "Brennan",
    "Sato",
    "Fairbairn",
    "Achebe",
    "Dunmore",
    "Varga",
    "Quill",
    "Rasmussen",
];
const PUBLISHERS: &[&str] = &[
    "Penguin Classics",
    "Faber & Faber",
    "Vintage",
    "Oxford World's Classics",
    "Picador",
    "New Directions",
    "Granta",
    "NYRB Classics",
];
/// Tags as a calibre user really types them: spaces, case, an apostrophe, a
/// quoted word, and one entry that is two tags joined by a comma — calibre
/// splits it, which is the wart.
const TAGS: &[&str] = &[
    "Fiction",
    "classics",
    "science fiction",
    "to-read",
    "Poetry",
    "history",
    "editor's pick",
    "\"favourite\"",
    "book club, 2024",
    "Philosophy",
    "gift",
    "signed copy",
];
const SERIES: &[&str] = &[
    "The Harbour Quartet",
    "Collected Works",
    "Northern Lights Cycle",
];

// ---- the plan ----------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct Row {
    /// Gutenberg id, or `None` for a formatless row.
    gutenberg: Option<u64>,
    title: String,
    authors: String,
    publisher: Option<String>,
    /// `Some(year)` or `None` = undated (calibre's `0101-01-01`).
    year: Option<i32>,
    tags: Vec<String>,
    series: Option<String>,
    /// `Some` only when the series is given an index; a series without one is
    /// left to calibre's 1.0 default, which is the wart under test.
    series_index: Option<f64>,
    isbn: Option<String>,
    goodreads: Option<u64>,
    /// Put the ISBN in through `identifiers:` (true) or the `isbn` field (false).
    isbn_via_identifiers: bool,
    language: Option<String>,
    rendered_cover: bool,
    convert_to: Option<&'static str>,
    salt: Option<Salt>,
    why: Option<&'static str>,
}

#[derive(Debug, Clone)]
struct Salt {
    case: String,
    field: &'static str,
    expect: String,
    normal: Option<String>,
    value: String,
}

fn below(rng: &mut ChaCha8Rng, n: usize) -> usize {
    (rng.next_u64() % n as u64) as usize
}

fn chance(rng: &mut ChaCha8Rng, percent: u64) -> bool {
    rng.next_u64() % 100 < percent
}

/// A valid ISBN-13 with a 978 prefix. Computed here, not by the engine's
/// `normalize_isbn` — the generator is the independent oracle.
fn isbn13(rng: &mut ChaCha8Rng) -> String {
    let mut digits = vec![9u32, 7, 8];
    for _ in 0..9 {
        digits.push(rng.next_u32() % 10);
    }
    let sum: u32 = digits
        .iter()
        .enumerate()
        .map(|(i, d)| if i % 2 == 0 { *d } else { d * 3 })
        .sum();
    digits.push((10 - sum % 10) % 10);
    digits
        .iter()
        .map(|d| char::from_digit(*d, 10).unwrap())
        .collect()
}

#[derive(serde::Deserialize)]
struct ManifestBook {
    id: u64,
    title: String,
    author: String,
    language: String,
}

#[derive(serde::Deserialize)]
struct Manifest {
    books: Vec<ManifestBook>,
}

/// Expand the bank's `{repeat:s:n}` template; refuse what cannot be an argv
/// element (NUL) or needs a per-row substitution (`{self_title}`).
fn usable_value(raw: &str) -> Option<String> {
    if raw.contains('\0') || raw.contains("{self_title}") {
        return None;
    }
    if let Some(inner) = raw
        .strip_prefix("{repeat:")
        .and_then(|r| r.strip_suffix('}'))
    {
        let (s, n) = inner.rsplit_once(':')?;
        return Some(s.repeat(n.parse().ok()?));
    }
    Some(raw.to_string())
}

fn hostile_candidates(field: &str) -> std::io::Result<Vec<Salt>> {
    let bank: Value = serde_json::from_str(&std::fs::read_to_string(hostile_path())?)?;
    let mut out = Vec::new();
    for case in bank["cases"].as_array().into_iter().flatten() {
        let fields = case["fields"].as_array().cloned().unwrap_or_default();
        if !fields.iter().any(|f| f.as_str() == Some(field)) {
            continue;
        }
        let Some(value) = case["value"].as_str().and_then(usable_value) else {
            continue;
        };
        out.push(Salt {
            case: case["id"].as_str().unwrap_or_default().to_string(),
            field: SALT_FIELDS.iter().find(|f| **f == field).unwrap(),
            expect: case["expect"].as_str().unwrap_or_default().to_string(),
            normal: case["normal"].as_str().map(str::to_string),
            value,
        });
    }
    Ok(out)
}

fn plan(manifest: &Manifest, epubs: &Path, seed: u64) -> std::io::Result<Vec<Row>> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed ^ 0xCA11_B4E0);
    let mut rows = Vec::new();

    let mut books: Vec<&ManifestBook> = manifest
        .books
        .iter()
        .filter(|b| epubs.join(format!("{}.epub", b.id)).is_file())
        .collect();
    books.sort_by_key(|b| b.id);

    for b in books {
        // calibre's own separator. The manifest lists the Federalist's three
        // authors with commas; a calibre user would have typed `&`.
        let authors = b.author.split(", ").collect::<Vec<_>>().join(" & ");
        let mut row = Row {
            gutenberg: Some(b.id),
            title: b.title.clone(),
            authors,
            language: Some(b.language.clone()),
            rendered_cover: RENDERED_COVERS.contains(&b.id),
            convert_to: CONVERSIONS.iter().find(|(id, _)| *id == b.id).map(|c| c.1),
            ..Default::default()
        };
        dress(&mut row, &mut rng, 1813..1926);
        rows.push(row);
    }

    // The second edition: a physical Penguin copy of a book the shelf already
    // holds as a Gutenberg epub. Same work, different edition, real ISBN.
    rows.push(Row {
        title: "Crime and Punishment".into(),
        authors: "Fyodor Dostoyevsky".into(),
        publisher: Some("Penguin Classics".into()),
        year: Some(2003),
        isbn: Some("9780140449136".into()),
        isbn_via_identifiers: true,
        goodreads: Some(7144),
        tags: vec!["classics".into()],
        why: Some("second edition of Gutenberg 2554 — duplicate detection"),
        ..Default::default()
    });

    let mut seen = BTreeSet::new();
    let mut invented = Vec::new();
    while rows.len() < rows.iter().filter(|r| r.gutenberg.is_some()).count() + 1 + PHYSICAL_ROWS {
        let title = if chance(&mut rng, 70) {
            format!(
                "The {} {}",
                ADJECTIVES[below(&mut rng, ADJECTIVES.len())],
                NOUNS[below(&mut rng, NOUNS.len())]
            )
        } else {
            let noun = NOUNS[below(&mut rng, NOUNS.len())];
            let article = if noun.starts_with(['A', 'E', 'I', 'O', 'U']) {
                "An"
            } else {
                "A"
            };
            format!(
                "{article} {noun} in {}",
                PLACES[below(&mut rng, PLACES.len())]
            )
        };
        if seen.contains(&title) {
            continue;
        }
        let mut person = || {
            format!(
                "{} {}",
                FIRST[below(&mut rng, FIRST.len())],
                LAST[below(&mut rng, LAST.len())]
            )
        };
        let mut authors = person();
        let two = person();
        if chance(&mut rng, 12) {
            authors = format!("{authors} & {two}");
        }
        // Two invented rows whose authors share a name must not share a title
        // word either: the importer reads *The Distant Atlas* by Noor Brennan
        // as a near-miss of *The Distant Island* by Helen Brennan (77%), and
        // *The Borrowed Clockmaker* by Mateo Dunmore as one of *The Borrowed
        // Orchard* by Elena Fairbairn & Mateo Hale (74%), and refuses to
        // create either, so a salted case on that row never reaches its book.
        // Re-drawn rather than kept as a collision; the deliberate duplicate is
        // the second Crime and Punishment above.
        let words = title_words(&title);
        let names = name_words(&authors);
        if invented
            .iter()
            .any(|(w, n): &(BTreeSet<String>, BTreeSet<String>)| {
                !w.is_disjoint(&words) && !n.is_disjoint(&names)
            })
        {
            continue;
        }
        seen.insert(title.clone());
        invented.push((words, names));
        let mut row = Row {
            title,
            authors,
            language: Some("en".into()),
            ..Default::default()
        };
        dress(&mut row, &mut rng, 1950..2024);
        rows.push(row);
    }

    salt(&mut rows, &mut rng)?;
    Ok(rows)
}

/// An invented title's words, without its articles and its `in`.
fn title_words(title: &str) -> BTreeSet<String> {
    title
        .split_whitespace()
        .filter(|w| !matches!(*w, "The" | "A" | "An" | "in"))
        .map(str::to_string)
        .collect()
}

/// Every first and last name in an invented `A & B` author field.
fn name_words(authors: &str) -> BTreeSet<String> {
    authors
        .split_whitespace()
        .filter(|w| *w != "&")
        .map(str::to_string)
        .collect()
}

/// The benign metadata every row gets, from the banks.
fn dress(row: &mut Row, rng: &mut ChaCha8Rng, years: std::ops::Range<i32>) {
    if chance(rng, 70) {
        row.year = Some(years.start + below(rng, (years.end - years.start) as usize) as i32);
    }
    if chance(rng, 50) {
        row.publisher = Some(PUBLISHERS[below(rng, PUBLISHERS.len())].to_string());
    }
    let ntags = below(rng, 4);
    let mut tags = BTreeSet::new();
    for _ in 0..ntags {
        tags.insert(TAGS[below(rng, TAGS.len())].to_string());
    }
    row.tags = tags.into_iter().collect();
    let s = below(rng, 100);
    if s < 12 {
        row.series = Some(SERIES[below(rng, SERIES.len())].to_string());
        // Novellas are numbered 1.5 in calibre; one in four here.
        let whole = 1 + below(rng, 5) as u32;
        row.series_index = Some(if chance(rng, 25) {
            f64::from(whole) + 0.5
        } else {
            f64::from(whole)
        });
    } else if s < 17 {
        row.series = Some(SERIES[below(rng, SERIES.len())].to_string());
    }
    let i = below(rng, 100);
    if i < 40 {
        row.isbn = Some(isbn13(rng));
        row.isbn_via_identifiers = chance(rng, 50);
        if chance(rng, 50) {
            row.goodreads = Some(1000 + rng.next_u64() % 9_000_000);
        }
    } else if i < 60 {
        // An identifiers map with no isbn: `isbn` comes back empty beside it.
        row.goodreads = Some(1000 + rng.next_u64() % 9_000_000);
    }
}

/// One case per salted field, on distinct rows, within 5% of the rows.
fn salt(rows: &mut [Row], rng: &mut ChaCha8Rng) -> std::io::Result<()> {
    let budget = rows.len() * 5 / 100;
    let mut used = BTreeSet::new();
    for field in SALT_FIELDS.iter().take(budget) {
        // Distinct cases: `empty-string` fits four of the five fields, and
        // salting it twice spends the 5% on one observation.
        let candidates: Vec<Salt> = hostile_candidates(field)?
            .into_iter()
            .filter(|c| {
                !rows
                    .iter()
                    .any(|r| r.salt.as_ref().is_some_and(|s| s.case == c.case))
            })
            .collect();
        if candidates.is_empty() {
            continue;
        }
        let pick = candidates[below(rng, candidates.len())].clone();
        // Salt formatless rows: they are the cheap ones, and a salted title on
        // a Gutenberg row would also change its on-disk folder name.
        let mut idx = below(rng, rows.len());
        let mut probed = 0;
        while rows[idx].gutenberg.is_some() || rows[idx].why.is_some() || used.contains(&idx) {
            idx = (idx + 1) % rows.len();
            probed += 1;
            if probed > rows.len() {
                return Err(std::io::Error::other(
                    "no formatless row left to salt: raise PHYSICAL_ROWS",
                ));
            }
        }
        used.insert(idx);
        rows[idx].salt = Some(pick);
    }
    Ok(())
}

// ---- running calibre ----------------------------------------------------------

struct Calibre {
    library: PathBuf,
    config: PathBuf,
}

impl Calibre {
    fn cmd(&self, sub: &str) -> Command {
        let mut c = Command::new("calibredb");
        c.env("CALIBRE_CONFIG_DIRECTORY", &self.config)
            .arg(sub)
            .arg(format!("--with-library={}", self.library.display()));
        c
    }

    fn run(&self, mut c: Command) -> std::io::Result<Result<String, String>> {
        let out = c.output()?;
        if out.status.success() {
            Ok(Ok(String::from_utf8_lossy(&out.stdout).into_owned()))
        } else {
            let err = String::from_utf8_lossy(&out.stderr);
            Ok(Err(err.lines().last().unwrap_or("").to_string()))
        }
    }

    fn must(&self, c: Command, what: &str) -> std::io::Result<String> {
        self.run(c)?
            .map_err(|e| std::io::Error::other(format!("calibredb {what}: {e}")))
    }
}

fn added_id(stdout: &str) -> std::io::Result<i64> {
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("Added book ids: "))
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| std::io::Error::other(format!("no id in calibredb output: {stdout}")))
}

fn refuse_foreign(out: &Path) -> std::io::Result<()> {
    let lib = out.join("library");
    if out.display().to_string().contains("Calibre Library") {
        return Err(std::io::Error::other(
            "refusing: the target looks like a user's calibre library",
        ));
    }
    // Anything already in library/ that this generator did not mark is not
    // ours to wipe, whether or not it carries a metadata.db.
    let foreign_contents =
        lib.is_dir() && !lib.join(MARKER).exists() && std::fs::read_dir(&lib)?.next().is_some();
    if foreign_contents {
        return Err(std::io::Error::other(format!(
            "refusing: {} holds files this generator did not create",
            lib.display()
        )));
    }
    Ok(())
}

pub fn generate(
    manifest: &Path,
    epubs: &Path,
    out: &Path,
    opts: &Options,
) -> std::io::Result<usize> {
    refuse_foreign(out)?;
    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(manifest)?)?;
    let rows = plan(&manifest, epubs, opts.seed)?;

    let lib = out.join("library");
    if lib.exists() {
        std::fs::remove_dir_all(&lib)?;
    }
    let work = out.join("work");
    if work.exists() {
        std::fs::remove_dir_all(&work)?;
    }
    std::fs::create_dir_all(&lib)?;
    std::fs::create_dir_all(&work)?;
    std::fs::write(
        lib.join(MARKER),
        "built by `cargo run -p corpus -- gen-calibre`\n",
    )?;
    let cal = Calibre {
        library: lib.clone(),
        config: out.join("calibre-config"),
    };
    std::fs::create_dir_all(&cal.config)?;

    let mut observed: BTreeMap<i64, Value> = BTreeMap::new();
    for (n, row) in rows.iter().enumerate() {
        let mut c = cal.cmd("add");
        c.arg("--duplicates");
        match row.gutenberg {
            Some(id) => {
                c.arg(epubs.join(format!("{id}.epub")));
            }
            None => {
                c.arg("--empty").arg(format!("--title={}", row.title));
            }
        }
        // `isbn` is not a `set_metadata` field; `add --isbn` is the tool's other
        // door for it, so the rows that do not use `identifiers:` use this one.
        if let Some(isbn) = row.isbn.as_ref().filter(|_| !row.isbn_via_identifiers) {
            c.arg(format!("--isbn={isbn}"));
        }
        let id = added_id(&cal.must(c, "add")?)?;
        assert_eq!(id, n as i64 + 1, "calibre ids follow plan order");

        let mut fields = vec![
            format!("title:{}", row.title),
            format!("authors:{}", row.authors),
            format!("tags:{}", row.tags.join(",")),
            format!(
                "pubdate:{}",
                row.year
                    .map(|y| format!("{y:04}-06-15"))
                    .unwrap_or_default()
            ),
        ];
        if let Some(p) = &row.publisher {
            fields.push(format!("publisher:{p}"));
        }
        if let Some(s) = &row.series {
            fields.push(format!("series:{s}"));
        }
        if let Some(i) = row.series_index {
            fields.push(format!("series_index:{i}"));
        }
        if let Some(l) = &row.language {
            fields.push(format!("languages:{l}"));
        }
        let mut ids = Vec::new();
        if let Some(isbn) = &row.isbn {
            // `identifiers:` replaces the whole map, so an ISBN that came in
            // through `add --isbn` is restated when a goodreads id joins it.
            if row.isbn_via_identifiers || row.goodreads.is_some() {
                ids.push(format!("isbn:{isbn}"));
            }
        }
        if let Some(g) = row.goodreads {
            ids.push(format!("goodreads:{g}"));
        }
        if !ids.is_empty() {
            fields.push(format!("identifiers:{}", ids.join(",")));
        }
        if row.rendered_cover {
            let png = work.join(format!("cover-{id}.png"));
            write_cover(&png, id)?;
            fields.push(format!("cover:{}", png.display()));
        }
        let mut c = cal.cmd("set_metadata");
        c.arg(id.to_string());
        for f in &fields {
            c.arg(format!("--field={f}"));
        }
        cal.must(c, "set_metadata")?;

        if let Some(ext) = row.convert_to {
            let gid = row.gutenberg.unwrap();
            let target = work.join(format!("{gid}.{ext}"));
            let status = Command::new("ebook-convert")
                .env("CALIBRE_CONFIG_DIRECTORY", &cal.config)
                .arg(epubs.join(format!("{gid}.epub")))
                .arg(&target)
                .output()?;
            if !status.status.success() {
                return Err(std::io::Error::other(format!(
                    "ebook-convert {gid} -> {ext} failed"
                )));
            }
            let mut c = cal.cmd("add_format");
            c.arg(id.to_string()).arg(&target);
            cal.must(c, "add_format")?;
        }

        // The salt goes last and alone, so a value calibre refuses costs only
        // itself and the row keeps everything above.
        if let Some(s) = &row.salt {
            let calibre_field = match s.field {
                "title" => "title",
                "author" => "authors",
                "tag" => "tags",
                "isbn" => "identifiers",
                _ => "pubdate",
            };
            let value = if s.field == "isbn" {
                format!("isbn:{}", s.value)
            } else {
                s.value.clone()
            };
            let mut c = cal.cmd("set_metadata");
            c.arg(id.to_string())
                .arg(format!("--field={calibre_field}:{value}"));
            let rejected = cal.run(c)?.err();
            observed.insert(
                id,
                json!({
                    "case": s.case,
                    "field": s.field,
                    "expect": s.expect,
                    "normal": s.normal,
                    "input": s.value,
                    "calibre_rejected": rejected,
                }),
            );
        }
    }

    // calibre's view, projected onto what is deterministic.
    let mut c = cal.cmd("list");
    c.args([
        "--for-machine",
        "--fields",
        "all",
        "--sort-by",
        "id",
        "--ascending",
    ]);
    let listing: Vec<Value> = serde_json::from_str(&cal.must(c, "list")?)?;
    let lock = lock(&rows, &listing, &observed, opts.seed)?;
    std::fs::write(
        out.join("calibre.lock.json"),
        serde_json::to_string_pretty(&lock)? + "\n",
    )?;
    std::fs::remove_dir_all(&work)?;
    Ok(rows.len())
}

fn ext_of(p: &str) -> String {
    Path::new(p)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn lock(
    rows: &[Row],
    listing: &[Value],
    observed: &BTreeMap<i64, Value>,
    seed: u64,
) -> std::io::Result<Value> {
    if listing.len() != rows.len() {
        return Err(std::io::Error::other(format!(
            "calibre lists {} rows, the plan has {}",
            listing.len(),
            rows.len()
        )));
    }
    let mut out_rows = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut bump = |k: &str| *counts.entry(k.to_string()).or_default() += 1;
    for (r, plan) in listing.iter().zip(rows) {
        let id = r["id"].as_i64().unwrap_or_default();
        let mut formats: Vec<String> = r["formats"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(ext_of)
            .collect();
        formats.sort();
        let mut tags: Vec<String> = r["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        tags.sort();
        let identifier_keys: Vec<String> = r["identifiers"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let year: Option<i64> = r["pubdate"]
            .as_str()
            .and_then(|d| d.get(..4))
            .and_then(|y| y.parse().ok());
        let has_cover = r["cover"].as_str().is_some_and(|c| !c.is_empty());
        let isbn = r["isbn"].as_str().unwrap_or_default().to_string();

        rows_counts(&mut bump, &formats, has_cover, &isbn, year, plan);
        let mut row = json!({
            "calibre_id": id,
            "title": r["title"],
            "authors": r["authors"],
            "formats": formats,
            "tags": tags,
            "series": r["series"],
            "series_index": r["series_index"],
            "has_cover": has_cover,
            "isbn": isbn,
            "identifier_keys": identifier_keys,
            "pubdate_year": year,
            "publisher": r["publisher"],
            "gutenberg": plan.gutenberg,
        });
        if let Some(why) = plan.why {
            row["why"] = json!(why);
        }
        if let Some(s) = observed.get(&id) {
            let mut s = s.clone();
            let field = s["field"].as_str().unwrap_or_default().to_string();
            s["calibre_reported"] = match field.as_str() {
                "title" => r["title"].clone(),
                "author" => r["authors"].clone(),
                "tag" => json!(tags),
                "isbn" => r["isbn"].clone(),
                _ => json!(year),
            };
            row["salted"] = s;
            bump("salted");
        }
        out_rows.push(row);
    }
    counts.insert("rows".into(), rows.len());
    Ok(json!({
        "$comment": [
            "Semantic lock for the calibre layer (see crates/corpus/src/calibre_lib.rs).",
            "calibredb writes metadata.db, minting uuids, timestamps and last_modified that",
            "no seed controls, and it renders pubdate through the local timezone. So this",
            "lock records what the engine must SEE, never bytes: per row the calibre id, title,",
            "authors as calibredb reports them, format extensions, tags, series, series_index,",
            "cover presence, isbn, identifier keys and the publication year. It is a function",
            "of (seed, generator_version, corpus/manifest.json); the database bytes are not.",
            "`salted` names the hostile.json case a row carries; `calibre_reported` is what",
            "calibre made of it and `calibre_rejected` is calibredb's own refusal, if any.",
            "Never resolve a change here by regenerating it blind: read the diff."
        ],
        "generator_version": GENERATOR_VERSION,
        "seed": seed,
        "counts": counts,
        "rows": out_rows,
    }))
}

fn rows_counts(
    bump: &mut impl FnMut(&str),
    formats: &[String],
    has_cover: bool,
    isbn: &str,
    year: Option<i64>,
    plan: &Row,
) {
    if formats.is_empty() {
        bump("formatless");
    }
    for f in formats {
        bump(&format!("format_{f}"));
    }
    if has_cover {
        bump("with_cover");
    }
    if !isbn.is_empty() {
        bump("with_isbn");
    }
    if year.is_some_and(|y| y < 1000) {
        bump("undated");
    }
    if plan.series.is_some() {
        bump("in_series");
    }
}

// ---- rendered covers ----------------------------------------------------------

const COVER_W: u32 = 400;
const COVER_H: u32 = 600;

/// A flat two-tone cover with a diagonal, deterministic from the calibre id —
/// `devdb.rs`'s approach, so a wrong tile is obvious rather than plausible.
fn write_cover(path: &Path, id: i64) -> std::io::Result<()> {
    let hue = ((id as f64) * 137.507_764_05) % 360.0;
    let (r, g, b) = hsv_rgb(hue, 0.50, 0.70);
    let (dr, dg, db) = hsv_rgb((hue + 180.0) % 360.0, 0.55, 0.35);
    let mut data = Vec::with_capacity((COVER_W * COVER_H * 3) as usize);
    for y in 0..COVER_H {
        for x in 0..COVER_W {
            let stripe = (x + y) % 120 < 18 || y > COVER_H * 4 / 5;
            let px = if stripe { [dr, dg, db] } else { [r, g, b] };
            data.extend_from_slice(&px);
        }
    }
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), COVER_W, COVER_H);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc
        .write_header()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    w.write_image_data(&data)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    w.finish().map_err(|e| std::io::Error::other(e.to_string()))
}

fn hsv_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let c = v * s;
    let hp = h / 60.0;
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let f = |n: f64| ((n + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (f(r), f(g), f(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        serde_json::from_str(
            &std::fs::read_to_string(crate::gutenberg::default_manifest()).unwrap(),
        )
        .unwrap()
    }

    /// The plan — which rows, which metadata, which salts — is a pure function
    /// of the seed. No calibre needed, so this runs everywhere.
    #[test]
    fn the_plan_is_a_function_of_the_seed() {
        let m = manifest();
        let none = Path::new("/nonexistent-epub-dir");
        let a = plan(&m, none, 42).unwrap();
        let b = plan(&m, none, 42).unwrap();
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
        let c = plan(&m, none, 43).unwrap();
        assert_ne!(format!("{a:?}"), format!("{c:?}"));
    }

    #[test]
    fn salting_stays_inside_five_percent_one_per_row() {
        let m = manifest();
        let rows = plan(&m, Path::new("/nonexistent-epub-dir"), 42).unwrap();
        let salted = rows.iter().filter(|r| r.salt.is_some()).count();
        assert!(salted >= 1, "something is salted");
        assert!(salted * 100 <= rows.len() * 5, "{salted} of {}", rows.len());
        let fields: BTreeSet<_> = rows
            .iter()
            .filter_map(|r| r.salt.as_ref())
            .map(|s| s.field)
            .collect();
        assert_eq!(
            fields.len(),
            salted,
            "one field per salted row, no field twice"
        );
    }

    #[test]
    fn generated_isbns_carry_a_valid_check_digit() {
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        for _ in 0..200 {
            let s = isbn13(&mut rng);
            let sum: u32 = s
                .chars()
                .enumerate()
                .map(|(i, c)| c.to_digit(10).unwrap() * if i % 2 == 0 { 1 } else { 3 })
                .sum();
            assert_eq!(sum % 10, 0, "{s}");
        }
    }

    #[test]
    fn a_foreign_library_is_refused() {
        let tmp = std::env::temp_dir().join(format!("rb-cal-refuse-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("library")).unwrap();
        std::fs::write(tmp.join("library/metadata.db"), b"x").unwrap();
        assert!(refuse_foreign(&tmp).is_err());
        std::fs::write(tmp.join("library").join(MARKER), b"").unwrap();
        assert!(refuse_foreign(&tmp).is_ok());
        // Unmarked contents without a metadata.db are refused too.
        std::fs::remove_dir_all(&tmp).unwrap();
        std::fs::create_dir_all(tmp.join("library")).unwrap();
        std::fs::write(tmp.join("library/notes.txt"), b"x").unwrap();
        assert!(refuse_foreign(&tmp).is_err());
        std::fs::remove_dir_all(&tmp).unwrap();
        assert!(refuse_foreign(Path::new("/home/x/Calibre Library/sub")).is_err());
    }

    #[test]
    fn repeat_templates_expand_and_nul_is_refused() {
        assert_eq!(usable_value("{repeat:ab:3}").unwrap(), "ababab");
        assert!(usable_value("a\u{0}b").is_none());
        assert!(usable_value("x [[{self_title}]]").is_none());
    }
}
