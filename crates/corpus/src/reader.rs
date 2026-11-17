//! Layer `reader`: `physical.csv` (a Goodreads export) and `verbs.jsonl`, one JSON object per line
//! `{argv, expect, case, why}` replayed in order by the recipe runner after importing the CSV.
//! Books are referenced by ISBN or a title fragment matching exactly one row, never by id: ids
//! depend on build order.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const GENERATOR_VERSION: u32 = 2;

/// The calendar runs from 2024-01-01 to `END_DAY` (2026-06-30); fixed, never `now()`.
const EPOCH_DAY: i64 = 19_723;
const END_DAY: i64 = 20_634;

pub struct Options {
    pub seed: u64,
}

pub fn default_out() -> PathBuf {
    repo_root().join("corpus/generated/reader")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn hostile_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("hostile.json")
}

/// The two *Meditations* are the duplicate-edition pair `merge` folds; *Middlemrch* is the typo
/// `set --title` corrects.
const INVENTED: [(&str, &str); 30] = [
    ("Meditations (Hays translation)", "Marcus Aurelius"),
    ("Meditations (Long translation)", "Marcus Aurelius"),
    ("Middlemrch", "George Eliot"),
    ("The Lighthouse Keeper's Almanac", "Maren Solvik"),
    ("Salt Roads of the Baltic", "Tomas Ruud"),
    ("A Field Guide to Small Weather", "Ines Okafor"),
    ("The Orchard at Vyborg", "Katya Belova"),
    ("Grammar of the Tide", "Hollis Reyes"),
    ("Notes Toward a Quiet House", "Elodie Marchetti"),
    ("The Interpreter's Daughter", "Ana Lucía Ferreira"),
    ("Second Ledger", "Pieter van der Berg"),
    ("Paper Lanterns, Iron Bridges", "Hana Takeda"),
    ("The Cartographer of Lost Rivers", "Owen Pryce"),
    ("Winter Physics", "Dagny Holm"),
    ("How to Mend a Net", "Siobhán Ní Bhriain"),
    ("The Bookbinder's Apprentice", "Ruth Adeyemi"),
    ("Letters from the Glasshouse", "Clara Whitlock"),
    ("An Atlas of Unfinished Cities", "Mateo Ilić"),
    ("The Night Ferry to Tallinn", "Kristjan Saar"),
    ("Small Gods of the Kitchen", "Priya Raman"),
    ("Thirty Bells", "Jun Okonkwo-Park"),
    ("The Patient Garden", "Margit Szabó"),
    ("Borrowed Light", "Esther Lindqvist"),
    ("On Walking Slowly", "Felix Brandauer"),
    ("The Hundred-Year Kettle", "Yusuf Demir"),
    ("A Short History of Rain", "Nell Garrow"),
    ("The Clockmaker's Census", "Bartholomew Fenn"),
    ("Moths in the Archive", "Isadora Quint"),
    ("The Stonemason's Year", "Gwilym Rhys"),
    ("Under the Linden, a Novel", "Ottilie Haas"),
];

const PUBLISHERS: [&str; 6] = [
    "Penguin Classics",
    "Oxford World's Classics",
    "Vintage",
    "Faber & Faber",
    "Little, Brown and Company",
    "NYRB Classics",
];
const BINDINGS: [&str; 4] = [
    "Paperback",
    "Hardcover",
    "Mass Market Paperback",
    "Kindle Edition",
];
const SHELF_TAGS: [&str; 6] = [
    "classics",
    "book-club",
    "owned",
    "borrowed",
    "favourites",
    "secondhand",
];

const REVIEW_OPENINGS: [&str; 8] = [
    "Slow to start, then it would not let go of me.",
    "I read this on the train, mostly, which suited it.",
    "Not what I expected, and better for it.",
    "The middle third drags, but the ending earns it.",
    "Read it for book club; argued about it for two hours.",
    "A reread, and it holds up, mostly.",
    "Beautiful sentences, thin plot, and I didn't mind.",
    "Picked up at a secondhand stall for two pounds.",
];
const REVIEW_MIDDLES: [&str; 8] = [
    "The line \"we are all of us in the gutter\" is not from this book, but it felt like it was.",
    "My copy has someone else's pencil marks in it, which made it a conversation.",
    "Chapter 7, the one with the storm, is the best thing in it.",
    "It is long, discursive, occasionally maddening, and I underlined half of it.",
    "She writes \"home\" the way other writers write \"war\".",
    "Three stars for the book, five for the last page.",
    "I kept a pencil in it, and a receipt as a bookmark, and both are still there.",
    "The translation is stiff in places, but the bones are good.",
];
const REVIEW_CLOSINGS: [&str; 6] = [
    "Would lend it, would want it back.",
    "Recommended, with reservations.",
    "I'll read it again in ten years and see who changed.",
    "Not for everyone; for me, yes.",
    "Glad I finished it, gladder it's over.",
    "Five stars, no notes. Well, some notes.",
];
const PRIVATE_NOTES: [&str; 6] = [
    "Borrowed from Mum, return by Easter.",
    "Library copy, due back the 14th; renewed twice.",
    "Signed copy, shelf above the desk.",
    "Lent to Sam, never came back.",
    "Bought at the Hay festival, 2024.",
    "Water damage on pp. 40-60, from the bath.",
];

const NOTE_REACTIONS: [&str; 10] = [
    "This passage reframes everything before it.",
    "Not sure I believe the narrator here.",
    "The structure mirrors the opening chapter almost exactly.",
    "Copied this out by hand; it reads differently that way.",
    "A question for book club: who is really speaking here?",
    "The weather is doing the emotional work again.",
    "This is the third time a door has been left open. Deliberate?",
    "Funny, and then suddenly not funny at all.",
    "I disagree with this, strongly, and that's the point of it.",
    "Margin note in pencil: \"yes\" underlined twice.",
];
const NOTE_TOPICS: [&str; 6] = [
    "first impressions",
    "the narrator",
    "a passage on memory",
    "structure",
    "an argument with it",
    "the ending",
];
const DANGLING: [&str; 4] = [
    "Unreliable Narrators",
    "Weather as Feeling",
    "Books I Lent Out",
    "Translation Questions",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shelf {
    Read,
    Reading,
    ToRead,
}

impl Shelf {
    fn goodreads(self) -> &'static str {
        match self {
            Shelf::Read => "read",
            Shelf::Reading => "currently-reading",
            Shelf::ToRead => "to-read",
        }
    }
}

#[derive(Clone, Debug)]
struct BookRow {
    gr_id: u32,
    title: String,
    author: String,
    extra_authors: String,
    frag: String,
    source: &'static str,
    isbn13: Option<String>,
    pages: u32,
    year: u32,
    orig_year: Option<u32>,
    publisher: &'static str,
    binding: &'static str,
    shelf: Shelf,
    start: Option<i64>,
    date_read: Option<i64>,
    date_added: i64,
    /// `None` is a blank cell: Goodreads writes `0`, a hand-edited export does not.
    rating: Option<u32>,
    review: Option<String>,
    private_notes: Option<String>,
    read_count: u32,
    tags: Vec<&'static str>,
}

#[derive(Serialize)]
struct Verb {
    argv: Vec<String>,
    expect: &'static str,
    case: Option<String>,
    why: String,
    #[serde(skip)]
    field: &'static str,
}

#[derive(Serialize)]
struct Salt {
    file: &'static str,
    row: usize,
    field: &'static str,
    case: String,
    expect: String,
}

#[derive(Clone)]
struct Hostile {
    id: String,
    fields: Vec<String>,
    value: String,
    expect: String,
}

fn load_hostile() -> std::io::Result<Vec<Hostile>> {
    let text = std::fs::read_to_string(hostile_path())?;
    let v: Value = serde_json::from_str(&text).map_err(std::io::Error::other)?;
    let cases = v["cases"].as_array().cloned().unwrap_or_default();
    Ok(cases
        .iter()
        .map(|c| Hostile {
            id: c["id"].as_str().unwrap_or_default().to_string(),
            fields: c["fields"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|f| f.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            value: c["value"].as_str().unwrap_or_default().to_string(),
            expect: c["expect"].as_str().unwrap_or_default().to_string(),
        })
        .collect())
}

pub fn expand(value: &str, self_title: &str) -> String {
    let mut v = value.replace("{self_title}", self_title);
    if let Some(rest) = v.strip_prefix("{repeat:")
        && let Some(body) = rest.strip_suffix('}')
        && let Some((s, n)) = body.rsplit_once(':')
        && let Ok(n) = n.parse::<usize>()
    {
        v = s.repeat(n);
    }
    v
}

fn case<'a>(bank: &'a [Hostile], id: &str) -> &'a Hostile {
    bank.iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("hostile.json has no case {id}"))
}

pub fn generate(manifest: &Path, out: &Path, opts: &Options) -> std::io::Result<usize> {
    std::fs::create_dir_all(out)?;
    let bank = load_hostile()?;
    let mut rng = ChaCha8Rng::seed_from_u64(opts.seed);
    let real = manifest_books(manifest)?;
    let mut books = build_books(&mut rng, &real);
    let mut salts = Vec::new();

    let csv_salted = salt_csv(&mut rng, &bank, &mut books, &mut salts);
    let csv = write_csv(&books)?;
    let verbs = build_verbs(&mut rng, &bank, &books, &csv_salted, &mut salts);

    let mut jsonl = String::new();
    for v in &verbs {
        jsonl.push_str(&serde_json::to_string(v).map_err(std::io::Error::other)?);
        jsonl.push('\n');
    }
    std::fs::write(out.join("physical.csv"), &csv)?;
    std::fs::write(out.join("verbs.jsonl"), &jsonl)?;

    let manifest_json = serde_json::json!({
        "$comment": "Layer `reader`: the physical reader. Generated by `cargo run -p corpus -- gen-reader`; see crates/corpus/src/reader.rs. `salted` tags every hostile row/line with its hostile.json case; `files` is the byte lock.",
        "generator_version": GENERATOR_VERSION,
        "seed": opts.seed,
        "files": {
            "physical.csv": { "rows": books.len(), "sha256": hex(&Sha256::digest(csv.as_bytes())) },
            "verbs.jsonl": { "lines": verbs.len(), "sha256": hex(&Sha256::digest(jsonl.as_bytes())) },
        },
        "books": books.iter().map(|b| serde_json::json!({
            "title": b.title,
            "frag": b.frag,
            "author": b.author,
            "shelf": b.shelf.goodreads(),
            "started": b.start.map(date),
            "date_read": b.date_read.map(date),
            "source": b.source,
            "read_count": b.read_count,
            "has_isbn": b.isbn13.is_some(),
            "isbn13": b.isbn13,
            "salted": csv_salted.contains(&b.gr_id),
        })).collect::<Vec<_>>(),
        "salted": salts,
    });
    let mut text = serde_json::to_string_pretty(&manifest_json).map_err(std::io::Error::other)?;
    text.push('\n');
    std::fs::write(out.join("manifest.json"), text)?;
    Ok(verbs.len())
}

fn manifest_books(manifest: &Path) -> std::io::Result<Vec<(String, String)>> {
    let text = std::fs::read_to_string(manifest)?;
    let v: Value = serde_json::from_str(&text).map_err(std::io::Error::other)?;
    Ok(v["books"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|b| {
            (
                b["title"].as_str().unwrap_or_default().to_string(),
                b["author"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect())
}

fn build_books(rng: &mut ChaCha8Rng, real: &[(String, String)]) -> Vec<BookRow> {
    // One manifest book per author: a second title by the same author can score into the importer's
    // candidate band against the first, and a refused row is never created, stranding its verbs.
    let mut authors = BTreeSet::new();
    let mut seeds: Vec<(String, String, &'static str)> = real
        .iter()
        .filter(|(_, a)| authors.insert(a.clone()))
        .take(30)
        .map(|(t, a)| (t.clone(), a.clone(), "manifest"))
        .collect();
    seeds.extend(
        INVENTED
            .iter()
            .map(|(t, a)| (t.to_string(), a.to_string(), "invented")),
    );

    let n = seeds.len();
    let mut order: Vec<usize> = (0..n).collect();
    shuffle(rng, &mut order);
    // Pinned roles go to fixed invented rows: both Meditations and Middlemrch are read, so `merge`
    // and `set` have history to act on.
    let pinned_read: BTreeSet<&str> = [
        "Meditations (Hays translation)",
        "Meditations (Long translation)",
        "Middlemrch",
    ]
    .into_iter()
    .collect();

    let mut books = Vec::with_capacity(n);
    let mut read_cursor = EPOCH_DAY + 7;
    let mut n_read = 0;
    let mut n_reading = 0;
    for (k, &i) in order.iter().enumerate() {
        let (title, author_field, source) = &seeds[i];
        let shelf = if pinned_read.contains(title.as_str()) || n_read < 26 {
            Shelf::Read
        } else if n_reading < 7 {
            Shelf::Reading
        } else {
            Shelf::ToRead
        };
        match shelf {
            Shelf::Read => n_read += 1,
            Shelf::Reading => n_reading += 1,
            Shelf::ToRead => {}
        }
        let (author, extra) = match author_field.split_once(", ") {
            Some((first, rest)) => (first.to_string(), rest.to_string()),
            None => (author_field.clone(), String::new()),
        };
        // A row reusing a corpus title always carries an ISBN: once layers share a library the title
        // names two editions, and verbs select by ISBN so composition stays unambiguous.
        let isbn13 = (*source == "manifest" || below(rng, 10) >= 2).then(|| isbn13(rng));
        let pages = 90 + below(rng, 800);
        let orig_year = 1600 + below(rng, 420);
        let (start, date_read, date_added, read_count) = match shelf {
            Shelf::Read => {
                let len = 5 + below(rng, 36) as i64;
                let start = read_cursor;
                read_cursor += len / 2 + below(rng, 12) as i64;
                let added = start - 3 - below(rng, 200) as i64;
                let rc = if k % 9 == 4 { 2 + below(rng, 2) } else { 1 };
                (Some(start), Some(start + len), added, rc)
            }
            Shelf::Reading => {
                let start = END_DAY - 10 - below(rng, 50) as i64;
                (Some(start), None, start - below(rng, 300) as i64, 1)
            }
            Shelf::ToRead => (None, None, EPOCH_DAY + below(rng, 900) as i64, 0),
        };
        let rating = match shelf {
            Shelf::Read => match below(rng, 14) {
                0 => None,
                1 => Some(0),
                r => Some(1 + r % 5),
            },
            _ if below(rng, 8) == 0 => None,
            _ => Some(0),
        };
        let review = (shelf == Shelf::Read && below(rng, 10) < 7).then(|| review(rng));
        let private_notes = (below(rng, 6) == 0).then(|| pick(rng, &PRIVATE_NOTES).to_string());
        let mut tags = vec![pick(rng, &SHELF_TAGS)];
        if below(rng, 3) == 0 {
            let t = pick(rng, &SHELF_TAGS);
            if !tags.contains(&t) {
                tags.push(t);
            }
        }
        books.push(BookRow {
            gr_id: 40_000_000 + (i as u32) * 7_919,
            // Where the row has a valid ISBN the verbs select by it: it survives composition with other
            // layers, a title fragment does not.
            frag: isbn13
                .as_deref()
                .filter(|i| is_valid_isbn13(i))
                .map(str::to_owned)
                .unwrap_or_else(|| title.clone()),
            title: title.clone(),
            author,
            extra_authors: extra,
            source,
            isbn13,
            pages,
            year: (orig_year + below(rng, 200)).min(2025),
            orig_year: (below(rng, 5) != 0).then_some(orig_year),
            publisher: pick(rng, &PUBLISHERS),
            binding: pick(rng, &BINDINGS),
            shelf,
            start,
            date_read,
            date_added: date_added.max(EPOCH_DAY - 400),
            rating,
            review,
            private_notes,
            read_count,
            tags,
        });
    }
    books
}

fn review(rng: &mut ChaCha8Rng) -> String {
    let mut parts = vec![pick(rng, &REVIEW_OPENINGS).to_string()];
    if below(rng, 3) != 0 {
        parts.push(pick(rng, &REVIEW_MIDDLES).to_string());
    }
    parts.push(pick(rng, &REVIEW_CLOSINGS).to_string());
    parts.join("\n")
}

/// Returns the salted rows' ids, which no verb selects (a salted title is not a stable fragment).
fn salt_csv(
    rng: &mut ChaCha8Rng,
    bank: &[Hostile],
    books: &mut [BookRow],
    salts: &mut Vec<Salt>,
) -> BTreeSet<u32> {
    // The ISBN salt rides in the verbs (`set --isbn`), so the CSV carries three rows.
    let plan: [(&'static str, &str); 3] = [
        ("csv-cell", "csv-formula-injection"),
        ("review", "html-img-onerror"),
        ("date", "date-invalid-day"),
    ];
    let budget = books.len() / 20;
    let mut salted = BTreeSet::new();
    // Only to-read and plain read rows that no verb names are eligible.
    let protected: BTreeSet<&str> = PROTECTED.into_iter().collect();
    let mut eligible: Vec<usize> = (0..books.len())
        .filter(|&i| books[i].shelf == Shelf::Read && !protected.contains(books[i].title.as_str()))
        .collect();
    shuffle(rng, &mut eligible);
    for (&(field, id), &row) in plan.iter().zip(eligible.iter()).take(budget) {
        let c = case(bank, id);
        assert!(c.fields.iter().any(|f| f == field || f == "csv-cell"));
        let b = &mut books[row];
        let value = expand(&c.value, &b.title);
        match field {
            "review" | "csv-cell" => b.review = Some(value),
            // The date cell is written raw by `write_csv` from this marker, carried in `private_notes`.
            "date" => {
                b.date_read = None;
                b.private_notes = Some(format!("\u{2}DATE:{value}"));
            }
            _ => unreachable!(),
        }
        salted.insert(b.gr_id);
        salts.push(Salt {
            file: "physical.csv",
            row: row + 1,
            field,
            case: c.id.clone(),
            expect: c.expect.clone(),
        });
    }
    salted
}

const PROTECTED: [&str; 3] = [
    "Meditations (Hays translation)",
    "Meditations (Long translation)",
    "Middlemrch",
];

const HEADER: [&str; 24] = [
    "Book Id",
    "Title",
    "Author",
    "Author l-f",
    "Additional Authors",
    "ISBN",
    "ISBN13",
    "My Rating",
    "Average Rating",
    "Publisher",
    "Binding",
    "Number of Pages",
    "Year Published",
    "Original Publication Year",
    "Date Read",
    "Date Added",
    "Bookshelves",
    "Bookshelves with positions",
    "Exclusive Shelf",
    "My Review",
    "Spoiler",
    "Private Notes",
    "Read Count",
    "Owned Copies",
];

/// Brackets a cell written raw after the csv writer quoted everything else: `="…"` armour is not
/// valid CSV, and a blanket `"""` replace would corrupt a review ending in a quotation.
const RAW: char = '\u{1}';

fn raw(s: &str) -> String {
    format!("{RAW}{s}{RAW}")
}

fn write_csv(books: &[BookRow]) -> std::io::Result<String> {
    let mut w = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .from_writer(Vec::new());
    w.write_record(HEADER)?;
    for (pos, b) in books.iter().enumerate() {
        let (isbn10, isbn13) = match &b.isbn13 {
            Some(i) if is_valid_isbn13(i) => (
                isbn13_to_10(i)
                    .map(|t| raw(&format!("=\"{t}\"")))
                    .unwrap_or_else(|| raw("=\"\"")),
                raw(&format!("=\"{i}\"")),
            ),
            Some(i) => (raw("=\"\""), raw(&format!("=\"{i}\""))),
            None => (raw("=\"\""), raw("=\"\"")),
        };
        let (date_read, private_notes) = match &b.private_notes {
            Some(p) if p.starts_with('\u{2}') => (
                p.trim_start_matches("\u{2}DATE:").to_string(),
                String::new(),
            ),
            other => (
                b.date_read.map(date).unwrap_or_default(),
                other.clone().unwrap_or_default(),
            ),
        };
        let shelves: Vec<&str> = b
            .tags
            .iter()
            .copied()
            .chain(std::iter::once(b.shelf.goodreads()))
            .filter(|s| *s != "read" && *s != "to-read" && *s != "currently-reading")
            .collect();
        let positions: Vec<String> = shelves
            .iter()
            .enumerate()
            .map(|(k, s)| format!("{s} (#{})", pos + k + 1))
            .collect();
        let rec: Vec<String> = vec![
            b.gr_id.to_string(),
            b.title.clone(),
            b.author.clone(),
            last_first(&b.author),
            b.extra_authors.clone(),
            isbn10,
            isbn13,
            b.rating.map(|r| r.to_string()).unwrap_or_default(),
            format!("{}.{:02}", 3 + (b.gr_id % 2), b.gr_id % 97),
            b.publisher.to_string(),
            b.binding.to_string(),
            b.pages.to_string(),
            b.year.to_string(),
            b.orig_year.map(|y| y.to_string()).unwrap_or_default(),
            date_read,
            date(b.date_added),
            shelves.join(", "),
            positions.join(", "),
            b.shelf.goodreads().to_string(),
            b.review.clone().unwrap_or_default(),
            String::new(),
            private_notes,
            b.read_count.to_string(),
            u32::from(b.tags.contains(&"owned")).to_string(),
        ];
        w.write_record(&rec)?;
    }
    let bytes = w
        .into_inner()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let text = String::from_utf8(bytes).map_err(std::io::Error::other)?;
    // The csv writer quoted each raw cell (it contains `"`); replace that form with the bare armour.
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(start) = rest.find(&format!("\"{RAW}")) {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1 + RAW.len_utf8()..];
        let end = after
            .find(&format!("{RAW}\""))
            .expect("a raw cell is closed");
        out.push_str(&after[..end].replace("\"\"", "\""));
        rest = &after[end + RAW.len_utf8() + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn last_first(author: &str) -> String {
    match author.rsplit_once(' ') {
        Some((first, last)) => format!("{last}, {first}"),
        None => author.to_string(),
    }
}

fn s(x: &str) -> String {
    x.to_string()
}

fn ok(argv: Vec<String>, why: impl Into<String>) -> Verb {
    Verb {
        argv,
        expect: "ok",
        case: None,
        why: why.into(),
        field: "",
    }
}

#[expect(clippy::too_many_lines)]
fn build_verbs(
    rng: &mut ChaCha8Rng,
    bank: &[Hostile],
    books: &[BookRow],
    csv_salted: &BTreeSet<u32>,
    salts: &mut Vec<Salt>,
) -> Vec<Verb> {
    let mut verbs: Vec<Verb> = Vec::new();
    let usable: Vec<&BookRow> = books
        .iter()
        .filter(|b| !csv_salted.contains(&b.gr_id))
        .collect();
    let reading: Vec<&BookRow> = usable
        .iter()
        .copied()
        .filter(|b| b.shelf == Shelf::Reading)
        .collect();
    let read: Vec<&BookRow> = usable
        .iter()
        .copied()
        .filter(|b| b.shelf == Shelf::Read && !PROTECTED.contains(&b.title.as_str()))
        .collect();

    // The importer asks about two rows that look like one book; the reader answers with `--new`,
    // which creates only the undecided rows. The path is repo-relative: the runner runs from the root.
    verbs.push(ok(
        vec![
            s("goodreads"),
            s("import"),
            s("corpus/generated/reader/physical.csv"),
            s("--new"),
        ],
        "answer the importer's duplicate question: create the undecided rows",
    ));

    for b in &reading {
        let mut page = 3 + below(rng, 20);
        let sessions = 5 + below(rng, 5);
        for _ in 0..sessions {
            if page >= b.pages {
                break;
            }
            verbs.push(ok(
                vec![s("progress"), b.frag.clone(), page.to_string()],
                format!("typed page for a physical copy of {}", b.title),
            ));
            page += 8 + below(rng, 40);
        }
    }
    if let Some(b) = reading.first() {
        verbs.push(ok(
            vec![
                s("progress"),
                b.frag.clone(),
                b.pages.to_string(),
                s("--finished"),
            ],
            "a physical book finished by hand: last page and --finished in one verb",
        ));
    }
    if let Some(b) = read.first() {
        verbs.push(ok(
            vec![s("progress"), b.frag.clone(), s("--reread")],
            "a reread of a finished paper book opens a second reading",
        ));
        for p in [14, 61, 118] {
            verbs.push(ok(
                vec![s("progress"), b.frag.clone(), p.to_string()],
                "typed pages on the second reading",
            ));
        }
    }

    let mut authors: Vec<&str> = usable
        .iter()
        .filter(|b| b.shelf != Shelf::ToRead)
        .map(|b| b.author.as_str())
        .collect();
    authors.sort_unstable();
    authors.dedup();
    let mut book_note_titles: Vec<(String, String)> = Vec::new();
    for b in usable.iter().filter(|b| b.shelf != Shelf::ToRead) {
        let n = 1 + below(rng, 3);
        for k in 0..n {
            let topic = NOTE_TOPICS[(k as usize + b.gr_id as usize) % NOTE_TOPICS.len()];
            let title = format!("{} — {topic}", b.title);
            let page = 1 + below(rng, b.pages.max(2) - 1);
            let mut body = format!(
                "{} (p. {page}) See [[{}]].",
                pick(rng, &NOTE_REACTIONS),
                b.author
            );
            if below(rng, 5) == 0 {
                body.push_str(&format!(" Also [[{}]].", pick(rng, &DANGLING)));
            }
            verbs.push(ok(
                vec![
                    s("note"),
                    s("--book"),
                    b.frag.clone(),
                    s("--title"),
                    title.clone(),
                    s("--page"),
                    page.to_string(),
                    s("--"),
                    body,
                ],
                "a paper reader's note: cites a page, links to the author's hub",
            ));
            book_note_titles.push((b.author.clone(), title));
        }
    }
    for a in &authors {
        let links: Vec<String> = book_note_titles
            .iter()
            .filter(|(au, _)| au == a)
            .map(|(_, t)| format!("[[{t}]]"))
            .collect();
        verbs.push(ok(
            vec![
                s("note"),
                s("--title"),
                a.to_string(),
                s("--"),
                format!("Hub for {a}. {}", links.join(" ")),
            ],
            "an author hub note: written after the book notes, so its links resolve",
        ));
    }

    // Reviews and reflections open without $EDITOR; the prose lives in the CSV's My Review.
    for b in read.iter().take(10) {
        let rating = 2 + below(rng, 4);
        verbs.push(ok(
            vec![
                s("review"),
                b.frag.clone(),
                s("--no-edit"),
                s("--rating"),
                rating.to_string(),
            ],
            "a finished paper book's review, opened with a rating",
        ));
    }
    for b in reading.iter().skip(1).take(4) {
        verbs.push(ok(
            vec![s("reflect"), b.frag.clone(), s("--no-edit")],
            "a reflection on a book still being read",
        ));
    }

    // Gap pins: the CLI takes a review/reflection body only via $EDITOR and `cite` only numeric
    // ids. These lines are expected to fail so the replay flags them when the CLI grows the form.
    if let Some(b) = reading.get(1) {
        verbs.push(Verb {
            argv: vec![
                s("note"),
                s("--kind"),
                s("reflection"),
                s("--book"),
                b.frag.clone(),
                s("--"),
                s("Slower going this week. I keep rereading the same chapter, \"on purpose\", \nand the margins are full of pencil."),
            ],
            expect: "fail",
            case: None,
            why: "gap pin: a reflection body typed on the command line; the CLI refuses \
                  (reflections open per reading, body via $EDITOR only)"
                .into(),
            field: "",
        });
    }
    if let Some(b) = read.first() {
        verbs.push(Verb {
            argv: vec![
                s("note"),
                s("--kind"),
                s("review"),
                s("--book"),
                b.frag.clone(),
                s("--"),
                s("Four stars, mostly for the last third. Recommended, with patience."),
            ],
            expect: "fail",
            case: None,
            why: "gap pin: a review body typed on the command line; the CLI refuses \
                  (reviews open per reading, body via $EDITOR only)"
                .into(),
            field: "",
        });
    }
    if let Some(a) = authors.first() {
        verbs.push(Verb {
            argv: vec![s("cite"), a.to_string()],
            expect: "fail",
            case: None,
            why: "gap pin: `cite` by note title (the author hub note); the CLI takes a \
                  numeric note id only, which depends on build order"
                .into(),
            field: "",
        });
    }

    for w in ["narrator", "weather", "pencil"] {
        verbs.push(ok(
            vec![s("find"), s("--"), s(w)],
            "the reader looks for their own words",
        ));
    }

    let salted = hostile_verbs(bank, &reading);
    // k salted lines among n benign plus 4 corrections must satisfy 20k <= n + k + 4.
    let budget = (verbs.len() + 4) / 19;
    // An ISBN-changing salt goes last: later verbs select that book by ISBN, and a mid-stream
    // `set --isbn` would strand them.
    let mut late = Vec::new();
    for (field, mut v) in salted.into_iter().take(budget) {
        v.field = field;
        if field == "isbn" {
            late.push(v);
            continue;
        }
        let at = below(rng, verbs.len() as u32 + 1) as usize;
        verbs.insert(at, v);
    }

    // Corrections go last so earlier verbs select the uncorrected titles.
    let year_book = read.get(1).map(|b| b.frag.clone()).unwrap_or_default();
    let isbn_book = read.get(2).map(|b| b.frag.clone()).unwrap_or_default();
    verbs.push(ok(
        vec![s("set"), s("Middlemrch"), s("--title"), s("Middlemarch")],
        "a title fix: the typo from the export, corrected by the reader",
    ));
    verbs.push(ok(
        vec![s("set"), isbn_book, s("--isbn"), s("978-0-14-143951-8")],
        "an ISBN typed from the copyright page, hyphenated",
    ));
    verbs.push(ok(
        vec![s("set"), year_book, s("--year"), s("1871")],
        "a year correction",
    ));
    verbs.push(ok(
        vec![
            s("merge"),
            s("Long translation"),
            s("Hays translation"),
            s("--yes"),
        ],
        "two editions of one work, folded into the one the reader kept",
    ));

    verbs.extend(late);

    for (i, v) in verbs.iter().enumerate() {
        if let Some(c) = &v.case {
            let h = case(bank, c);
            salts.push(Salt {
                file: "verbs.jsonl",
                row: i + 1,
                field: v.field,
                case: c.clone(),
                expect: h.expect.clone(),
            });
        }
    }
    verbs
}

fn hostile_verbs(bank: &[Hostile], reading: &[&BookRow]) -> Vec<(&'static str, Verb)> {
    let frag = reading
        .get(1)
        .or(reading.first())
        .map(|b| b.frag.clone())
        .unwrap_or_default();
    let class_expect = |id: &str| -> &'static str {
        if case(bank, id).expect == "rejected" {
            "fail"
        } else {
            "ok"
        }
    };
    let body = |id: &str, title: &str| -> (&'static str, Verb) {
        let c = case(bank, id);
        (
            "note-body",
            Verb {
                argv: vec![
                    s("note"),
                    s("--title"),
                    s(title),
                    s("--"),
                    expand(&c.value, title),
                ],
                expect: class_expect(id),
                case: Some(c.id.clone()),
                why: format!("hostile note body ({}), passed after --", c.expect),
                field: "",
            },
        )
    };
    let title = |id: &str| -> (&'static str, Verb) {
        let c = case(bank, id);
        (
            "note-title",
            Verb {
                argv: vec![
                    s("note"),
                    s("--title"),
                    expand(&c.value, ""),
                    s("--"),
                    s("A note whose title is the hostile part."),
                ],
                expect: class_expect(id),
                case: Some(c.id.clone()),
                why: format!("hostile note title ({}) becomes a vault slug", c.expect),
                field: "",
            },
        )
    };
    let page = |id: &str| -> (&'static str, Verb) {
        let c = case(bank, id);
        (
            "page",
            Verb {
                argv: vec![s("progress"), frag.clone(), s("--"), c.value.clone()],
                expect: class_expect(id),
                case: Some(c.id.clone()),
                why: format!(
                    "a hostile typed page ({}); after -- so a leading minus is a value",
                    c.expect
                ),
                field: "",
            },
        )
    };
    let link = |id: &str, t: &str| ("wikilink", body(id, t).1);
    let isbn = {
        // Not `isbn-10-x-check`: the calibre layer salts that one too, and the same ISBN on a second
        // book collides on `books.isbn_10 UNIQUE` once the layers share a library.
        let c = case(bank, "isbn-979");
        (
            "isbn",
            Verb {
                argv: vec![s("set"), frag.clone(), s("--isbn"), c.value.clone()],
                expect: class_expect(&c.id),
                case: Some(c.id.clone()),
                why: "a hyphenated ISBN-10 with an X check digit, typed by hand".into(),
                field: "",
            },
        )
    };
    vec![
        body("argv-looking", "Argv in a body"),
        title("path-traversal-title"),
        page("page-negative"),
        page("page-text"),
        link("wikilink-traversal", "Traversal link"),
        body("html-script", "Script in a note"),
        isbn,
        title("windows-reserved"),
        link("wikilink-self", "A note that cites itself"),
        page("page-huge"),
        body("argv-double-dash", "Double dash in a body"),
        title("argv-short-flag"),
        body("fts-operators", "FTS syntax in a body"),
        body("frontmatter-breaker", "A body with a fence"),
    ]
}

fn date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}/{m:02}/{d:02}")
}

fn isbn13(rng: &mut ChaCha8Rng) -> String {
    let mut digits: Vec<u32> = vec![9, 7, 8];
    for _ in 0..9 {
        digits.push(below(rng, 10));
    }
    let sum: u32 = digits
        .iter()
        .enumerate()
        .map(|(i, d)| if i % 2 == 0 { *d } else { 3 * d })
        .sum();
    digits.push((10 - sum % 10) % 10);
    digits
        .iter()
        .map(|d| char::from_digit(*d, 10).unwrap())
        .collect()
}

fn is_valid_isbn13(s: &str) -> bool {
    s.len() == 13
        && s.bytes().all(|c| c.is_ascii_digit())
        && s.bytes()
            .enumerate()
            .map(|(i, c)| u32::from(c - b'0') * if i % 2 == 0 { 1 } else { 3 })
            .sum::<u32>()
            % 10
            == 0
}

fn isbn13_to_10(isbn13: &str) -> Option<String> {
    let body = &isbn13.strip_prefix("978")?[..9];
    let sum: u32 = body
        .chars()
        .enumerate()
        .map(|(i, c)| (10 - i as u32) * c.to_digit(10).unwrap())
        .sum();
    let check = (11 - sum % 11) % 11;
    let check = if check == 10 {
        'X'
    } else {
        char::from_digit(check, 10)?
    };
    Some(format!("{body}{check}"))
}

fn below(rng: &mut ChaCha8Rng, n: u32) -> u32 {
    rng.next_u32() % n
}

fn pick<'a>(rng: &mut ChaCha8Rng, from: &[&'a str]) -> &'a str {
    from[below(rng, from.len() as u32) as usize]
}

fn shuffle<T>(rng: &mut ChaCha8Rng, v: &mut [T]) {
    for i in (1..v.len()).rev() {
        let j = below(rng, i as u32 + 1) as usize;
        v.swap(i, j);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(seed: u64) -> tempfile::TempDir {
        let out = tempfile::tempdir().unwrap();
        generate(
            &repo_root().join("corpus/manifest.json"),
            out.path(),
            &Options { seed },
        )
        .unwrap();
        out
    }

    #[test]
    fn same_seed_same_bytes() {
        let a = build(42);
        let b = build(42);
        for f in ["physical.csv", "verbs.jsonl", "manifest.json"] {
            assert_eq!(
                std::fs::read(a.path().join(f)).unwrap(),
                std::fs::read(b.path().join(f)).unwrap(),
                "{f} differs between two runs of one seed"
            );
        }
    }

    #[test]
    fn csv_has_goodreads_header_and_24_columns() {
        let out = build(42);
        let text = std::fs::read_to_string(out.path().join("physical.csv")).unwrap();
        let recorded = std::fs::read_to_string(
            repo_root().join("crates/engine/tests/fixtures/goodreads/recorded/library-export.csv"),
        )
        .unwrap();
        assert_eq!(text.lines().next(), recorded.lines().next());
        assert!(text.contains("\r\n"), "Goodreads writes CRLF");
        assert!(text.contains(",=\""), "ISBNs carry Excel armour");
        // The armour is not valid CSV, so count columns as the engine's reader will; `csv` is lenient.
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(text.as_bytes());
        let mut n = 0;
        for rec in rdr.records() {
            let rec = rec.unwrap();
            assert_eq!(rec.len(), 24, "row {n} has {} columns", rec.len());
            n += 1;
        }
        assert!((40..=60).contains(&n), "{n} rows");
    }

    #[test]
    fn every_fragment_selects_exactly_one_book() {
        let out = build(42);
        let m: Value = serde_json::from_str(
            &std::fs::read_to_string(out.path().join("manifest.json")).unwrap(),
        )
        .unwrap();
        let mut titles: Vec<String> = m["books"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["title"].as_str().unwrap().to_lowercase())
            .collect();
        let isbns: Vec<String> = m["books"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| b["isbn13"].as_str().map(str::to_owned))
            .collect();
        let verbs = std::fs::read_to_string(out.path().join("verbs.jsonl")).unwrap();
        for line in verbs.lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            let argv: Vec<&str> = v["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap())
                .collect();
            let sel: Vec<&str> = match argv[0] {
                "progress" | "review" | "reflect" | "set" => vec![argv[1]],
                "merge" => vec![argv[1], argv[2]],
                "note" => argv
                    .iter()
                    .position(|a| *a == "--book")
                    .map(|i| vec![argv[i + 1]])
                    .unwrap_or_default(),
                _ => vec![],
            };
            for frag in sel {
                let is_isbn = frag.len() == 13 && frag.chars().all(|c| c.is_ascii_digit());
                let hits = if is_isbn {
                    isbns.iter().filter(|i| *i == frag).count()
                } else {
                    titles
                        .iter()
                        .filter(|t| t.contains(&frag.to_lowercase()))
                        .count()
                };
                assert_eq!(hits, 1, "{frag:?} selects {hits} books in {line}");
            }
            if argv[0] == "set"
                && let Some(i) = argv.iter().position(|a| *a == "--title")
            {
                let old = argv[1].to_lowercase();
                for t in titles.iter_mut().filter(|t| **t == old) {
                    *t = argv[i + 1].to_lowercase();
                }
            }
        }
    }

    #[test]
    fn salts_stay_inside_the_budget_and_name_real_cases() {
        let out = build(42);
        let m: Value = serde_json::from_str(
            &std::fs::read_to_string(out.path().join("manifest.json")).unwrap(),
        )
        .unwrap();
        let bank = load_hostile().unwrap();
        let rows = m["files"]["physical.csv"]["rows"].as_u64().unwrap() as usize;
        let lines = m["files"]["verbs.jsonl"]["lines"].as_u64().unwrap() as usize;
        let salts = m["salted"].as_array().unwrap();
        let in_csv = salts.iter().filter(|s| s["file"] == "physical.csv").count();
        let in_verbs = salts.iter().filter(|s| s["file"] == "verbs.jsonl").count();
        assert!(in_csv * 20 <= rows, "{in_csv} salted of {rows} rows");
        assert!(in_verbs * 20 <= lines, "{in_verbs} salted of {lines} lines");
        assert!(
            in_verbs >= 8,
            "the verbs carry the hostile layer: {in_verbs}"
        );
        for s in salts {
            let id = s["case"].as_str().unwrap();
            assert!(bank.iter().any(|c| c.id == id), "unknown case {id}");
        }
    }

    #[test]
    fn hostile_bank_ids_are_unique_and_classes_known() {
        let bank = load_hostile().unwrap();
        let mut seen = BTreeSet::new();
        for c in &bank {
            assert!(seen.insert(c.id.clone()), "duplicate id {}", c.id);
            assert!(
                ["verbatim", "normalised", "rejected", "no-crash"].contains(&c.expect.as_str()),
                "{} has class {}",
                c.id,
                c.expect
            );
        }
    }

    #[test]
    fn expand_handles_both_templates() {
        assert_eq!(expand("{repeat:ab:3}", ""), "ababab");
        assert_eq!(expand("x [[{self_title}]]", "T"), "x [[T]]");
    }
}
