//! Every committed EPUB, read through the engine's adapter and `epubkit`,
//! against a committed snapshot of the answers.
//!
//! The books are the 49 Gutenberg epubs under `corpus/epub/` and every
//! generated `*.epub` under `tests/fixtures/`. Per book the snapshot records
//! what an import and the chapter list read — `epub_info` (isbn, title,
//! authors, language), the table of contents as (label, depth, target,
//! spine_index), and the cover's sha256 and length — or that the book did not
//! open.
//!
//! **The snapshot was first written while the GPL `epub =2.1.4` crate was
//! still in the workspace**, with `epub_differential.rs` green: that test
//! compared every one of these books' metadata map, manifest, spine, cover
//! bytes and chapter list old-against-new and found them equal. So the golden
//! is the old crate's answers, and this test is what replaced the
//! differential when the old crate left. A change here is a
//! change to what an import of one of these books would store.
//!
//! Regenerate with `UPDATE_GOLDEN=1` (or `make golden`) — and read the diff:
//! a difference is a behaviour change, not noise.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bookmole::epub::{TocEntry, epub_info, table_of_contents};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const GOLDEN: &str = "tests/fixtures/epub_corpus.golden.json";
const FIXTURES_PREFIX: &str = "crates/engine/tests/fixtures/";

fn skip(reason: &str) {
    if std::env::var("BOOKMOLE_REQUIRE_FIXTURES").is_ok() {
        panic!("BOOKMOLE_REQUIRE_FIXTURES set but {reason}");
    }
    eprintln!("SKIPPED: {reason}");
}

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn epubs_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            epubs_under(&p, out);
        } else if p.extension().is_some_and(|x| x == "epub") {
            out.push(p);
        }
    }
}

/// The book's key in the golden: its path from the repository root, with `/`.
fn key(path: &Path) -> String {
    let repo = manifest_dir()
        .join("../..")
        .canonicalize()
        .expect("the repository root resolves");
    let path = path.canonicalize().expect("a listed epub resolves");
    let rel = path.strip_prefix(&repo).unwrap_or(&path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn answers(path: &Path) -> Value {
    let info = match epub_info(path) {
        Ok(i) => json!({
            "isbn": i.isbn,
            "title": i.title,
            "authors": i.authors,
            "language": i.language,
        }),
        Err(_) => return json!({ "opens": false }),
    };
    let toc: Vec<Value> = table_of_contents(path)
        .expect("a book whose metadata reads has a readable chapter list")
        .into_iter()
        .map(
            |TocEntry {
                 label,
                 depth,
                 target,
                 spine_index,
             }| json!([label, depth, target, spine_index]),
        )
        .collect();
    let cover = epubkit::Package::open(path)
        .expect("a book whose metadata reads opens")
        .cover()
        .map(|c| json!({ "sha256": hex(&Sha256::digest(&c.bytes)), "len": c.bytes.len() }));
    json!({ "opens": true, "info": info, "toc": toc, "cover": cover })
}

/// The golden's text: one book per block, one chapter per line, so a diff
/// reads as the chapters that changed rather than as JSON punctuation.
fn render(books: &BTreeMap<String, Value>) -> String {
    let c = |v: &Value| serde_json::to_string(v).expect("serialize");
    let mut out = String::from("{\n");
    for (i, (k, v)) in books.iter().enumerate() {
        out.push_str(&format!(
            "  {}: {{\n    \"opens\": {}",
            c(&json!(k)),
            c(&v["opens"])
        ));
        if v["opens"] == true {
            out.push_str(&format!(",\n    \"info\": {}", c(&v["info"])));
            out.push_str(&format!(",\n    \"cover\": {}", c(&v["cover"])));
            let rows = v["toc"].as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                out.push_str(",\n    \"toc\": []");
            } else {
                let rows: Vec<String> = rows.iter().map(|r| format!("      {}", c(r))).collect();
                out.push_str(&format!(",\n    \"toc\": [\n{}\n    ]", rows.join(",\n")));
            }
        }
        out.push_str("\n  }");
        out.push_str(if i + 1 < books.len() { ",\n" } else { "\n" });
    }
    out.push_str("}\n");
    out
}

#[test]
fn every_committed_epub_reads_as_the_golden_says() {
    let update = std::env::var("UPDATE_GOLDEN").is_ok();
    let corpus = manifest_dir().join("../../corpus/epub");
    let corpus_present = corpus.is_dir();

    let mut books = Vec::new();
    if corpus_present {
        epubs_under(&corpus, &mut books);
    } else {
        skip("corpus/epub is missing — only tests/fixtures' epubs are compared");
    }
    let fixtures_before = books.len();
    epubs_under(&manifest_dir().join("tests/fixtures"), &mut books);
    assert!(
        books.len() > fixtures_before,
        "tests/fixtures carries no epub — `make synthetic` generates them"
    );

    let actual: BTreeMap<String, Value> = books.iter().map(|p| (key(p), answers(p))).collect();
    let golden_path = manifest_dir().join(GOLDEN);

    if update {
        assert!(
            corpus_present,
            "UPDATE_GOLDEN with corpus/epub missing would drop 49 books from the golden"
        );
        std::fs::write(&golden_path, render(&actual)).expect("write golden");
        eprintln!(
            "UPDATE_GOLDEN: rewrote {} with {} books",
            golden_path.display(),
            actual.len()
        );
        return;
    }

    let raw = std::fs::read_to_string(&golden_path)
        .unwrap_or_else(|e| panic!("{}: {e} — run `make golden`", golden_path.display()));
    let mut expected: BTreeMap<String, Value> = serde_json::from_str(&raw).expect("parse golden");
    if !corpus_present {
        expected.retain(|k, _| k.starts_with(FIXTURES_PREFIX));
    }

    let mut failures = Vec::new();
    for (k, want) in &expected {
        match actual.get(k) {
            None => failures.push(format!("{k}: in the golden, not on disk")),
            Some(got) if got != want => failures.push(format!(
                "{k}: differs\n--- golden ---\n{}\n--- now ---\n{}",
                serde_json::to_string_pretty(want).unwrap_or_default(),
                serde_json::to_string_pretty(got).unwrap_or_default()
            )),
            Some(_) => {}
        }
    }
    for k in actual.keys().filter(|k| !expected.contains_key(*k)) {
        failures.push(format!(
            "{k}: on disk, not in the golden — run `make golden`"
        ));
    }
    eprintln!("compared {} epubs", actual.len());
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// The snapshot must be able to fail: it has to hold books that answer
/// something, or it would agree with a reader that answered nothing.
#[test]
fn the_golden_holds_real_answers() {
    let raw = std::fs::read_to_string(manifest_dir().join(GOLDEN)).expect("golden is committed");
    let golden: BTreeMap<String, Value> = serde_json::from_str(&raw).expect("parse golden");
    let opened = golden.values().filter(|v| v["opens"] == true).count();
    let with_toc = golden
        .values()
        .filter(|v| v["toc"].as_array().is_some_and(|t| !t.is_empty()))
        .count();
    let with_cover = golden.values().filter(|v| v["cover"].is_object()).count();
    let with_isbn = golden
        .values()
        .filter(|v| v["info"]["isbn"].is_string())
        .count();
    assert!(golden.len() >= 51, "{} books", golden.len());
    assert!(opened >= 51 && with_toc >= 50 && with_cover >= 1 && with_isbn >= 1);
}
