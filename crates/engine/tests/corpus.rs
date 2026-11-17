//! Tier-2 corpus tests over sidecars derived from Gutenberg epubs. Every skip is loud, and
//! `BOOKMOLE_REQUIRE_FIXTURES=1` turns it into a failure.

use std::path::{Path, PathBuf};

use bookmole::koreader::{self, KoStatus, parse_sidecar};
use bookmole::{Book, Storage};

mod common;

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/generated")
}

/// One subtree per sidecar layout; each is a plausible device library alone, but not together.
const LAYOUTS: [&str; 2] = ["modern", "legacy"];

fn sidecars() -> Option<Vec<PathBuf>> {
    let dir = corpus_dir();
    if !dir.is_dir() {
        return None;
    }
    let found = koreader::find_sidecars(&dir).ok()?;
    (!found.is_empty()).then_some(found)
}

fn skipped(reason: &str) -> bool {
    common::skipped(reason, "run `make corpus`")
}

macro_rules! corpus_or_skip {
    () => {
        match sidecars() {
            Some(s) => s,
            None => {
                skipped("the tier-2 corpus has not been generated");
                return;
            }
        }
    };
}

#[test]
fn generated_sidecars_match_the_lock_file() {
    let _ = corpus_or_skip!();
    let lock_path = corpus_dir().join("corpus.lock.json");
    let Ok(raw) = std::fs::read_to_string(&lock_path) else {
        skipped("corpus.lock.json is missing");
        return;
    };
    let lock: serde_json::Value = serde_json::from_str(&raw).expect("parse corpus.lock.json");

    let mut checked = 0;
    for (rel, want) in lock["files"].as_object().expect("files table") {
        if rel.starts_with("epub/") {
            continue;
        }
        let path = corpus_dir().join(rel);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        let got = {
            use sha2::{Digest, Sha256};
            Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        assert_eq!(
            got,
            want.as_str().unwrap(),
            "{rel} does not match the lock — regenerate rather than hand-editing"
        );
        checked += 1;
    }
    assert!(checked > 0, "the lock recorded no sidecars");
}

#[test]
fn every_generated_sidecar_parses() {
    let found = corpus_or_skip!();
    let mut total_highlights = 0;

    for path in &found {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let sc = parse_sidecar(&src)
            .unwrap_or_else(|e| panic!("{} failed to parse: {e}", path.display()));

        assert!(
            sc.title.is_some(),
            "{}: doc_props.title went missing",
            path.display()
        );
        for h in &sc.highlights {
            assert!(
                !h.text.is_empty(),
                "{}: empty highlight text",
                path.display()
            );
            assert!(
                h.pos0.is_some(),
                "{}: highlight without pos0",
                path.display()
            );
        }
        total_highlights += sc.highlights.len();
    }

    eprintln!(
        "corpus: {} sidecars, {total_highlights} highlights",
        found.len()
    );
    assert!(total_highlights > 0, "the corpus produced no highlights");
}

/// One layout per pass, because a device carries one: both trees hold the same annotations with
/// different payloads, so together every pass would refresh every row and never skip.
#[tokio::test]
async fn the_whole_corpus_imports_idempotently() {
    let _ = corpus_or_skip!();

    for layout in LAYOUTS {
        let dir = corpus_dir().join(layout);
        // Loud, not `continue`: silently covering nothing is what the skip rule prevents.
        let found = koreader::find_sidecars(&dir).unwrap_or_default();
        if found.is_empty() {
            skipped(&format!(
                "corpus/generated/{layout} holds no sidecars (regenerate: `make corpus`)"
            ));
            return;
        }

        let storage = Storage::connect("sqlite::memory:").await.expect("db");
        for path in &found {
            let src = std::fs::read_to_string(path).unwrap();
            let Ok(sc) = parse_sidecar(&src) else {
                continue;
            };
            let Some(title) = sc.title.clone() else {
                continue;
            };
            storage
                .upsert_book(
                    &Book {
                        title: Some(title),
                        authors: sc.authors.clone().into_iter().collect(),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .expect("seed");
        }

        let first = koreader::import(&storage, &dir, false)
            .await
            .expect("first corpus import");
        assert!(
            !first.imported.is_empty(),
            "{layout}: nothing matched; warnings: {:?}",
            first.warnings
        );

        let inserted: usize = first.imported.iter().map(|s| s.inserted).sum();
        let seen: usize = first.imported.iter().map(|s| s.inserted + s.skipped).sum();
        assert!(inserted > 0, "{layout}: corpus import inserted nothing");

        let mut before = Vec::new();
        for s in &first.imported {
            before.push((s.book_id, storage.list_highlights(s.book_id).await.unwrap()));
        }

        let second = koreader::import(&storage, &dir, false)
            .await
            .expect("second corpus import");
        for s in &second.imported {
            assert_eq!(
                s.inserted, 0,
                "{layout}: re-import inserted rows for {}",
                s.book_title
            );
            // Asserts `updated == 0` at scale: identical bytes must not look like a device edit.
            assert_eq!(
                s.updated, 0,
                "{layout}: re-import refreshed device fields for {} — the sidecar did not change",
                s.book_title
            );
        }
        let second_skipped: usize = second.imported.iter().map(|s| s.skipped).sum();
        assert_eq!(
            seen, second_skipped,
            "{layout}: re-import did not skip every entry it saw"
        );

        for (book_id, rows) in before {
            let now = storage.list_highlights(book_id).await.unwrap();
            assert_eq!(
                rows.len(),
                now.len(),
                "{layout}: row count changed for book {book_id}"
            );
            for (a, b) in rows.iter().zip(now.iter()) {
                assert_eq!(a.text, b.text, "{layout}: highlight text mutated");
                assert_eq!(a.page, b.page, "{layout}: page mutated");
                assert_eq!(a.ko_datetime, b.ko_datetime, "{layout}: datetime mutated");
            }
        }

        eprintln!(
            "corpus/{layout}: {inserted} highlights across {} books",
            first.imported.len()
        );
    }
}

#[test]
fn the_modern_and_legacy_encodings_agree_on_the_text() {
    let _ = corpus_or_skip!();
    let mut compared = 0;

    let Ok(modern_side) = koreader::find_sidecars(&corpus_dir().join("modern")) else {
        skipped("the modern corpus tree is missing");
        return;
    };

    for modern in &modern_side {
        let dir = modern.parent().unwrap();
        let name = dir.file_name().unwrap().to_str().unwrap();
        let legacy = corpus_dir().join("legacy").join(name).join(
            modern
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("metadata.epub.lua"),
        );
        if !legacy.is_file() {
            continue;
        }

        let a = parse_sidecar(&std::fs::read_to_string(modern).unwrap()).unwrap();
        let b = parse_sidecar(&std::fs::read_to_string(&legacy).unwrap()).unwrap();

        let mut ta: Vec<&str> = a.highlights.iter().map(|h| h.text.as_str()).collect();
        let mut tb: Vec<&str> = b.highlights.iter().map(|h| h.text.as_str()).collect();
        ta.sort_unstable();
        tb.sort_unstable();
        assert_eq!(
            ta, tb,
            "{name}: the two layouts disagree about the highlight text"
        );
        assert_eq!(a.title, b.title, "{name}: titles disagree");
        compared += 1;
    }

    assert!(compared > 0, "no modern/legacy pairs found");
    eprintln!("corpus: compared {compared} modern/legacy pairs");
}

fn layout_manifest(layout: &str) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(corpus_dir().join(layout).join("manifest.json")).ok()?;
    Some(serde_json::from_str(&raw).expect("layout manifest parses"))
}

macro_rules! manifest_or_skip {
    ($layout:expr) => {
        match layout_manifest($layout) {
            Some(m) => m,
            None => {
                skipped(&format!(
                    "corpus/generated/{}/manifest.json is missing (a pre-v3 corpus?)",
                    $layout
                ));
                return;
            }
        }
    };
}

fn salted_values(man: &serde_json::Value, sidecar: &str) -> Vec<String> {
    man["salted"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["sidecar"] == sidecar)
        .filter_map(|s| s["value"].as_str().map(str::to_string))
        .collect()
}

fn sidecar_path(layout: &str, slug: &str) -> PathBuf {
    corpus_dir()
        .join(layout)
        .join(format!("{slug}.sdr"))
        .join("metadata.epub.lua")
}

const TERMINAL: &[char] = &['.', '?', '!', '。', '！', '？'];
const CLOSING: &[char] = &['"', '\'', '”', '’', '»', '」', '』', '›'];

#[test]
fn every_highlight_is_a_sentence_or_a_single_word() {
    let _ = corpus_or_skip!();
    for layout in LAYOUTS {
        let man = manifest_or_skip!(layout);
        let (mut sentences, mut words) = (0usize, 0usize);
        for sc_row in man["sidecars"].as_array().expect("sidecars") {
            let slug = sc_row["sidecar"].as_str().unwrap();
            let salted = salted_values(&man, slug);
            let src = std::fs::read_to_string(sidecar_path(layout, slug)).unwrap();
            let sc = parse_sidecar(&src).unwrap();
            for h in &sc.highlights {
                if salted.iter().any(|v| v == &h.text) {
                    continue;
                }
                if !h.text.is_empty() && h.text.chars().all(char::is_alphabetic) {
                    words += 1;
                    continue;
                }
                let t = h.text.trim_end_matches(CLOSING);
                let spaceless = h.text.chars().any(is_spaceless_script);
                assert!(
                    t.ends_with(TERMINAL) && (spaceless || h.text.split_whitespace().count() >= 2),
                    "{layout}/{slug}: highlight is not a whole sentence: {:?}",
                    h.text
                );
                sentences += 1;
            }
        }
        let total = sentences + words;
        assert!(total > 0, "{layout}: no highlights");
        // Target is 5-10% single words, measured in tenths of a percent.
        let per_mille = words * 1000 / total;
        assert!(
            (50..=100).contains(&per_mille),
            "{layout}: {words} single-word highlights of {total} ({per_mille}‰)"
        );
    }
}

fn is_spaceless_script(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF)
}

#[test]
fn every_sidecar_has_a_consistent_reading_state() {
    let found = corpus_or_skip!();
    let mut by_status = std::collections::BTreeMap::<String, usize>::new();
    for path in &found {
        let sc = parse_sidecar(&std::fs::read_to_string(path).unwrap()).unwrap();
        let where_ = path.display();
        let summary = sc
            .summary
            .as_ref()
            .unwrap_or_else(|| panic!("{where_}: no summary"));
        let status = summary
            .status
            .clone()
            .unwrap_or_else(|| panic!("{where_}: no summary.status"));
        assert!(!status.is_unknown(), "{where_}: unknown status {status}");
        assert!(summary.modified.is_some(), "{where_}: no summary.modified");
        let pct = sc
            .percent_finished
            .unwrap_or_else(|| panic!("{where_}: no percent"));
        assert!((0.0..=1.0).contains(&pct), "{where_}: percent {pct}");
        assert_eq!(
            status == KoStatus::Complete,
            pct == 1.0,
            "{where_}: status {status} disagrees with percent {pct}"
        );
        let pages = sc
            .doc_pages
            .unwrap_or_else(|| panic!("{where_}: no doc_pages"));
        let stats = sc
            .stats
            .as_ref()
            .unwrap_or_else(|| panic!("{where_}: no stats"));
        assert_eq!(
            stats.pages,
            Some(pages),
            "{where_}: stats.pages != doc_pages"
        );
        assert!(
            stats.title.is_some() && stats.language.is_some(),
            "{where_}: stats incomplete"
        );
        let md5 = sc
            .partial_md5
            .as_deref()
            .unwrap_or_else(|| panic!("{where_}: no md5"));
        assert!(
            md5.len() == 32 && md5.chars().all(|c| c.is_ascii_hexdigit()),
            "{where_}: md5 {md5}"
        );
        for h in &sc.highlights {
            if let Some(p) = h.page {
                assert!(p >= 1 && p <= pages, "{where_}: page {p} of {pages}");
            }
        }
        *by_status.entry(status.to_string()).or_default() += 1;
    }
    eprintln!("corpus: sidecar statuses {by_status:?}");
    assert!(by_status.contains_key("reading") && by_status.contains_key("complete"));
}

async fn imported(layout: &str) -> Option<(Storage, koreader::ImportReport)> {
    let dir = corpus_dir().join(layout);
    let found = koreader::find_sidecars(&dir)
        .ok()
        .filter(|f| !f.is_empty())?;
    let storage = Storage::connect("sqlite::memory:").await.expect("db");
    for path in &found {
        let Ok(sc) = parse_sidecar(&std::fs::read_to_string(path).unwrap()) else {
            continue;
        };
        let Some(title) = sc.title.clone() else {
            continue;
        };
        storage
            .upsert_book(
                &Book {
                    title: Some(title),
                    authors: sc.authors.clone().into_iter().collect(),
                    ..Default::default()
                },
                None,
            )
            .await
            .expect("seed");
    }
    let report = koreader::import(&storage, &dir, false)
        .await
        .expect("import");
    Some((storage, report))
}

#[tokio::test]
async fn every_complete_sidecar_imports_as_a_finished_reading() {
    let _ = corpus_or_skip!();
    for layout in LAYOUTS {
        let man = manifest_or_skip!(layout);
        let Some((storage, _)) = imported(layout).await else {
            skipped(&format!("corpus/generated/{layout} holds no sidecars"));
            return;
        };
        let mut checked = 0;
        for row in man["sidecars"].as_array().unwrap() {
            if row["status"] != "complete" {
                continue;
            }
            let slug = row["sidecar"].as_str().unwrap();
            let md5 = row["partial_md5"].as_str().unwrap();
            let book = storage
                .find_book_by_partial_md5(md5)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("{layout}/{slug}: never linked to a book"));
            let readings = storage.list_readings(book.id.unwrap()).await.unwrap();
            assert!(
                readings.iter().any(|r| r.finished_at.is_some()),
                "{layout}/{slug}: complete on the device, but no finished reading: {readings:?}"
            );
            checked += 1;
        }
        assert!(checked > 0, "{layout}: the corpus has no complete sidecar");
        eprintln!("corpus/{layout}: {checked} complete sidecars finished");
    }
}

#[derive(serde::Deserialize)]
struct StatsExpected {
    books: Vec<StatsBook>,
}

#[derive(serde::Deserialize)]
struct StatsBook {
    title: String,
    md5: Option<String>,
    days: Vec<StatsDay>,
}

#[derive(serde::Deserialize)]
struct StatsDay {
    day: String,
    pages: i64,
    minutes: i64,
}

#[tokio::test]
async fn the_statistics_layer_imports_to_the_generators_totals() {
    use sqlx::Connection;
    let _ = corpus_or_skip!();
    for layout in LAYOUTS {
        let settings = corpus_dir()
            .join("stats")
            .join(layout)
            .join("koreader/settings");
        let (Ok(sql), Ok(exp)) = (
            std::fs::read_to_string(settings.join("statistics.sql")),
            std::fs::read_to_string(settings.join("expected.json")),
        ) else {
            skipped(&format!("corpus/generated/stats/{layout} is missing"));
            return;
        };
        let exp: StatsExpected = serde_json::from_str(&exp).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("statistics.sqlite3");
        {
            let opts = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&db)
                .create_if_missing(true);
            let mut conn = sqlx::SqliteConnection::connect_with(&opts).await.unwrap();
            sqlx::raw_sql(&sql)
                .execute(&mut conn)
                .await
                .expect("the plugin DDL applies");
            conn.close().await.unwrap();
        }

        let (storage, _) = imported(layout).await.expect("sidecars present");
        let report = bookmole::ko_statistics::import_statistics(&storage, &db)
            .await
            .unwrap();
        assert_eq!(report.books_in_db, exp.books.len(), "{layout}");

        // Two editions of one title match the same seeded book by title, so their measured days are one
        // book's days (observed behaviour, pinned).
        let mut per_book: std::collections::BTreeMap<i64, (Vec<String>, Vec<&StatsDay>)> =
            Default::default();
        for b in &exp.books {
            let md5 = b.md5.as_deref().unwrap();
            let Some(book) = storage.find_book_by_partial_md5(md5).await.unwrap() else {
                continue;
            };
            let e = per_book.entry(book.id.unwrap()).or_default();
            e.0.push(b.title.clone());
            e.1.extend(b.days.iter());
        }
        let mut checked = 0;
        for (id, (titles, mut days)) in per_book {
            days.sort_by(|a, b| a.day.cmp(&b.day));
            let got: Vec<_> = storage
                .reading_events(id)
                .await
                .unwrap()
                .into_iter()
                .filter(|e| e.source == "koreader")
                .collect();
            let got_days: Vec<&str> = got.iter().map(|e| e.day.as_str()).collect();
            let mut want_days: Vec<&str> = days.iter().map(|d| d.day.as_str()).collect();
            want_days.dedup();
            assert_eq!(got_days, want_days, "{layout}/{titles:?}: measured days");
            if want_days.len() == days.len() {
                for (g, w) in got.iter().zip(&days) {
                    assert_eq!(
                        g.minutes,
                        Some(w.minutes),
                        "{layout}/{titles:?} on {}",
                        w.day
                    );
                    assert_eq!(g.pages, Some(w.pages), "{layout}/{titles:?} on {}", w.day);
                }
            }
            let hl_days: std::collections::BTreeSet<String> = storage
                .list_highlights(id)
                .await
                .unwrap()
                .into_iter()
                .filter_map(|h| h.ko_datetime.map(|d| d[..10].to_string()))
                .collect();
            for d in &want_days {
                assert!(
                    hl_days.contains(*d),
                    "{layout}/{titles:?}: a statistics session on {d} with no highlight that day"
                );
            }
            checked += titles.len();
        }
        assert!(
            checked * 10 >= exp.books.len() * 9,
            "{layout}: only {checked} of {} statistics books matched a library book",
            exp.books.len()
        );
        eprintln!("corpus/stats/{layout}: {checked} books' measured days agree");
    }
}

#[tokio::test]
async fn salted_cases_reach_their_declared_class() {
    let _ = corpus_or_skip!();
    for layout in LAYOUTS {
        let man = manifest_or_skip!(layout);
        let salted = man["salted"].as_array().cloned().unwrap_or_default();
        assert!(!salted.is_empty(), "{layout}: nothing salted");
        let (storage, report) = imported(layout).await.expect("sidecars present");
        let _ = report;
        let mut outcomes = Vec::new();
        for s in &salted {
            let slug = s["sidecar"].as_str().unwrap();
            let field = s["field"].as_str().unwrap();
            let case = s["case"].as_str().unwrap();
            let expect = s["expect"].as_str().unwrap();
            let value = s["value"].as_str().unwrap();
            let src = std::fs::read_to_string(sidecar_path(layout, slug)).unwrap();
            let sc = parse_sidecar(&src).unwrap_or_else(|e| {
                panic!("{layout}/{slug} [{case}]: salted sidecar does not parse: {e}")
            });
            let parsed: Vec<Option<String>> = match field {
                "title" => vec![sc.title.clone()],
                "author" => vec![sc.authors.clone()],
                "highlight" => sc.highlights.iter().map(|h| Some(h.text.clone())).collect(),
                "ko-note" | "lua-string" => sc.highlights.iter().map(|h| h.note.clone()).collect(),
                other => panic!("{layout}/{slug} [{case}]: unexpected field {other}"),
            };
            let verbatim_parse = parsed.iter().any(|p| p.as_deref() == Some(value));
            match expect {
                "verbatim" => {
                    assert!(
                        verbatim_parse,
                        "{layout}/{slug} [{case}] {field}: not read back verbatim"
                    );
                    if field == "highlight" {
                        let md5 = sc.partial_md5.as_deref().unwrap();
                        let book = storage.find_book_by_partial_md5(md5).await.unwrap();
                        let book =
                            book.unwrap_or_else(|| panic!("{layout}/{slug} [{case}]: unmatched"));
                        let rows = storage.list_highlights(book.id.unwrap()).await.unwrap();
                        assert!(
                            rows.iter().any(|h| h.text == value),
                            "{layout}/{slug} [{case}]: highlight not stored verbatim"
                        );
                    }
                }
                "no-crash" => {}
                other => panic!(
                    "{layout}/{slug} [{case}]: this layer has no arm for `{other}` yet — add one"
                ),
            }
            outcomes.push(format!(
                "{case}/{field}: {}",
                if verbatim_parse {
                    "verbatim"
                } else {
                    "altered"
                }
            ));
        }
        eprintln!("corpus/{layout} salted outcomes: {outcomes:?}");
    }
}
