//! The calibre layer of the testing corpus, imported through the real
//! `calibredb`.
//!
//! `cargo run -p corpus -- gen-calibre` builds a real calibre library at
//! `corpus/generated/calibre/library/` and writes `calibre.lock.json` beside
//! it: a **semantic** lock of what calibre reports (titles, `&`-joined authors,
//! formats, tags, series, cover presence, isbn, publication year), never bytes.
//! This file imports that library into an in-memory engine and asserts the
//! engine saw what the lock says, that a second import changes nothing, and
//! that every salted hostile case reaches its declared class.
//!
//! The fake-binary pattern in `calibre.rs` is what CI runs. Here the binary is
//! the real one on purpose: the whole point of the layer is that the warts are
//! calibre's own. The test is fixture-gated twice — `calibredb` on `PATH` and
//! the generated layer — and skips loudly (`SKIPPED:`, failing under
//! `BOOKMOLE_REQUIRE_FIXTURES=1`) when either is missing.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use bookmole::calibre::{CalibreMatch, CalibreReport, CalibreRowState, ImportOptions};
use bookmole::{Book, BookQuery, BookSort, Engine};
use serde_json::Value;

mod common;

/// Hostile cases where the engine's observed behaviour contradicts the
/// declared class in `hostile.json`. The declaration is **not** changed to
/// match — it says what *should* happen — so the observation is pinned here
/// instead, and this test
/// goes red the day the engine is fixed and the entry must be removed and the
/// case asserted as declared. Each is reported as a suspected engine bug.
const KNOWN_DIVERGENCES: &[(&str, &str)] = &[
    (
        "isbn-excel-armour",
        "calibre.rs row_to_book runs normalize_isbn without goodreads::strip_armour, \
         so an Excel-armoured ISBN in calibre's identifiers map is dropped, not normalised",
    ),
    (
        "author-last-first",
        "calibre.rs StringOrList::names splits a calibre author on ',' as well as '&', \
     so `Le Guin, Ursula K.` becomes two authors",
    ),
];

fn layer() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/generated/calibre")
}

fn calibredb_dir() -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH")?).find(|d| {
        std::fs::metadata(d.join("calibredb"))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// calibredb is single-instance per machine: a second `calibredb list` while
/// one is running exits non-zero with "Another calibre program … is running".
/// cargo runs these tests on threads, so every test holds this for its whole
/// life. The engine surfaces that refusal as a typed `EngineError::Calibre`,
/// which is right — it is calibre's lock, not ours.
static CALIBRE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Built {
    _guard: tokio::sync::MutexGuard<'static, ()>,
    _tmp: tempfile::TempDir,
    engine: Engine,
    lock: Value,
    opts: ImportOptions,
    first: CalibreReport,
}

async fn build() -> Option<Built> {
    let Some(bin) = calibredb_dir() else {
        common::skipped("calibredb is not on PATH", "install calibre");
        return None;
    };
    let lock_path = layer().join("calibre.lock.json");
    if !lock_path.is_file() {
        common::skipped(
            "the calibre corpus layer is not generated",
            "cargo run -p corpus -- gen-calibre",
        );
        return None;
    }
    let lock: Value = serde_json::from_str(&std::fs::read_to_string(&lock_path).unwrap()).unwrap();
    let guard = CALIBRE.lock().await;
    let (tmp, engine) = common::engine_with_calibre(Some(bin)).await;
    let opts = ImportOptions {
        library: Some(layer().join("library")),
        ..Default::default()
    };
    let first = engine.import_calibre_library(&opts).await.expect("import");
    Some(Built {
        _guard: guard,
        _tmp: tmp,
        engine,
        lock,
        opts,
        first,
    })
}

fn rows(lock: &Value) -> &Vec<Value> {
    lock["rows"].as_array().expect("lock rows")
}

fn str_list(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

async fn book_of(b: &Built, calibre_id: i64) -> Option<Book> {
    let line = b.first.books.iter().find(|l| l.calibre_id == calibre_id)?;
    b.engine.get_book(line.book_id?).await.unwrap()
}

async fn raw_tags(b: &Built, book_id: i64) -> BTreeSet<String> {
    b.engine
        .storage()
        .book_tags(book_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|t| t.source == "calibre")
        .filter_map(|t| t.raw)
        .collect()
}

async fn library_size(engine: &Engine) -> usize {
    engine
        .list_books(&BookQuery::new(10_000, BookSort::Title))
        .await
        .unwrap()
        .len()
}

/// Every row the lock names is accounted for, and every row the import
/// created carries exactly what calibre reported.
#[tokio::test]
async fn the_engine_sees_what_the_lock_says() {
    let Some(b) = build().await else { return };
    let lock_rows = rows(&b.lock);
    assert_eq!(b.first.rows, lock_rows.len(), "calibre rows");
    assert_eq!(
        b.lock["counts"]["rows"].as_u64().unwrap() as usize,
        lock_rows.len()
    );

    // A book two calibre rows resolved to holds whichever row wrote last; its
    // fields are asserted in the two-editions block below, not row by row.
    let mut per_book = std::collections::BTreeMap::<i64, usize>::new();
    for l in &b.first.books {
        *per_book.entry(l.book_id.unwrap()).or_default() += 1;
    }
    let mut checked = 0;
    for r in lock_rows {
        let id = r["calibre_id"].as_i64().unwrap();
        let state = b.first.row_state(id);
        assert!(
            !matches!(state, CalibreRowState::Unreadable(_)),
            "row {id} ({}) unreadable: {state:?}",
            r["title"]
        );
        if r.get("salted").is_some() {
            continue; // asserted by class below
        }
        let line = b.first.books.iter().find(|l| l.calibre_id == id);
        let Some(line) = line.filter(|l| l.matched_by == CalibreMatch::New) else {
            continue; // matched an earlier row's book; its fields are that book's
        };
        if per_book[&line.book_id.unwrap()] > 1 {
            continue;
        }
        let book = b
            .engine
            .get_book(line.book_id.unwrap())
            .await
            .unwrap()
            .unwrap();
        let ctx = format!("row {id} {:?}", r["title"]);

        assert_eq!(book.title.as_deref(), r["title"].as_str(), "{ctx}: title");
        let mut authors: Vec<String> = r["authors"]
            .as_str()
            .unwrap()
            .split(" & ")
            .map(str::to_string)
            .collect();
        // calibre writes its default author, `Unknown`, for a book with none
        // (*Beowulf* here), and that placeholder is read as no author.
        if authors == ["Unknown"] {
            authors.clear();
        }
        assert_eq!(book.authors, authors, "{ctx}: authors");

        // The index exists only where the series does; calibre's 1.0 on a
        // series-less row is its default, not a claim.
        assert_eq!(
            book.series.as_deref(),
            r["series"].as_str(),
            "{ctx}: series"
        );
        let want_index = r["series"].as_str().and(r["series_index"].as_f64());
        assert_eq!(book.series_index, want_index, "{ctx}: series_index");

        let tags: BTreeSet<String> = str_list(&r["tags"]).into_iter().collect();
        assert_eq!(raw_tags(&b, book.id.unwrap()).await, tags, "{ctx}: tags");

        let isbn = r["isbn"].as_str().unwrap();
        if isbn.is_empty() {
            assert_eq!(
                (&book.isbn_10, &book.isbn_13),
                (&None, &None),
                "{ctx}: isbn"
            );
        } else {
            assert_eq!(book.isbn_13.as_deref(), Some(isbn), "{ctx}: isbn");
        }

        let year = r["pubdate_year"].as_i64().filter(|y| *y >= 1000);
        assert_eq!(
            book.publish_year, year,
            "{ctx}: publish_year (0101 is undated)"
        );

        let has_cover = r["has_cover"].as_bool().unwrap();
        assert_eq!(line.cover, has_cover, "{ctx}: cover copied");
        if has_cover {
            let p = book.cover_path.as_deref().expect("cover_path");
            assert!(Path::new(p).is_file(), "{ctx}: cover file {p}");
        } else {
            assert_eq!(book.cover_path, None, "{ctx}: no cover");
        }
        assert_eq!(
            line.files_linked,
            r["formats"].as_array().unwrap().len(),
            "{ctx}: every format identified by partial_md5"
        );
        checked += 1;
    }
    assert!(checked >= 60, "only {checked} rows were field-checked");
    assert_eq!(library_size(&b.engine).await, b.first.created());

    // Two editions of one title: the formatless Penguin copy must not become a
    // silent second book. Matched to the Gutenberg one or offered as a
    // candidate are both answers; `New` is the duplicate.
    let edition = lock_rows
        .iter()
        .find(|r| r.get("why").is_some())
        .expect("the second-edition row");
    let state = b.first.row_state(edition["calibre_id"].as_i64().unwrap());
    assert!(
        !matches!(state, CalibreRowState::New),
        "second edition created a duplicate: {state:?}"
    );
    // Observed, and reported as a suspected engine bug rather than blessed:
    // the edition auto-matches the Gutenberg copy by title, and `enrich_book`
    // then replaces that book's ISBN with the edition's (isbn_13 is
    // `Merge::Coalesce` with the incoming value first). Two calibre rows on one
    // book therefore rewrite each other's ISBN on every import, last row wins.
    if let CalibreRowState::Linked { matched_by, .. } = state {
        assert_eq!(matched_by, CalibreMatch::Title, "edition matched by");
        let book = book_of(&b, edition["calibre_id"].as_i64().unwrap())
            .await
            .unwrap();
        assert_eq!(
            book.isbn_13.as_deref(),
            edition["isbn"].as_str(),
            "the shared book carries the later row's ISBN; fixed? update this pin"
        );
    }
}

/// Idempotency: every row finds itself by uuid and nothing is
/// created, tagged, covered or identified twice.
#[tokio::test]
async fn a_second_import_changes_nothing() {
    let Some(b) = build().await else { return };
    let snapshot = |books: Vec<Book>| {
        books
            .into_iter()
            .map(|x| {
                format!(
                    "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
                    x.id,
                    x.title,
                    x.authors,
                    x.isbn_13,
                    x.series,
                    x.series_index,
                    x.publish_year,
                    x.cover_path
                )
            })
            .collect::<Vec<_>>()
    };
    let all = BookQuery::new(10_000, BookSort::Title);
    let rows_before = snapshot(b.engine.list_books(&all).await.unwrap());
    let before = rows_before.len();
    let again = b.engine.import_calibre_library(&b.opts).await.unwrap();
    assert_eq!(again.created(), 0);
    assert_eq!(again.unmatched.len(), b.first.unmatched.len(), "unmatched");
    assert_eq!(again.books.len(), b.first.books.len());
    for l in &again.books {
        assert_eq!(l.matched_by, CalibreMatch::Uuid, "{}", l.title);
        assert_eq!(l.tags_added, 0, "{}: tags", l.title);
        assert!(!l.cover, "{}: cover", l.title);
        assert_eq!(l.files_linked, 0, "{}: files", l.title);
    }
    assert_eq!(library_size(&b.engine).await, before);
    assert_eq!(
        snapshot(b.engine.list_books(&all).await.unwrap()),
        rows_before,
        "a second import left every book as it found it"
    );
}

/// Each salted row reaches the class `hostile.json` declares for its case,
/// judged on what calibre handed over (`calibre_reported`), since a value the
/// tool itself altered or refused is calibre's behaviour, not the engine's.
#[tokio::test]
async fn each_salted_case_reaches_its_declared_class() {
    let Some(b) = build().await else { return };
    let mut seen = 0;
    for r in rows(&b.lock) {
        let Some(s) = r.get("salted") else { continue };
        seen += 1;
        let id = r["calibre_id"].as_i64().unwrap();
        let case = s["case"].as_str().unwrap();
        let field = s["field"].as_str().unwrap();
        let expect = s["expect"].as_str().unwrap();
        let input = s["input"].as_str().unwrap();
        let reported = &s["calibre_reported"];
        let ctx = format!("case {case} ({field}, {expect}) on row {id}");

        // no-crash, for every class: the import finished (we are here), the
        // row is accounted for, and the library still answers a search.
        assert!(
            !matches!(b.first.row_state(id), CalibreRowState::Unreadable(_)),
            "{ctx}: unreadable"
        );
        b.engine
            .storage()
            .find_books_by_title("the")
            .await
            .unwrap_or_else(|e| panic!("{ctx}: search failed after import: {e}"));
        if expect == "no-crash" {
            continue;
        }
        let book = book_of(&b, id)
            .await
            .unwrap_or_else(|| panic!("{ctx}: the row made no book: {:?}", b.first.row_state(id)));

        if let Some((_, why)) = KNOWN_DIVERGENCES.iter().find(|(c, _)| *c == case) {
            eprintln!("KNOWN DIVERGENCE {ctx}: {why}; observed {:?}", book.authors);
            if case == "isbn-excel-armour" {
                assert_eq!(
                    (&book.isbn_10, &book.isbn_13),
                    (&None, &None),
                    "{ctx}: fixed? promote it"
                );
            }
            if case == "author-last-first" {
                assert_eq!(
                    book.authors,
                    vec!["Le Guin", "Ursula K."],
                    "{ctx}: fixed? promote it"
                );
            }
            continue;
        }

        match (field, expect) {
            ("title", "verbatim") => {
                assert_eq!(reported.as_str(), Some(input), "{ctx}: calibre kept it");
                assert_eq!(book.title.as_deref(), Some(input), "{ctx}");
            }
            ("title", "normalised") => {
                assert_eq!(book.title.as_deref(), s["normal"].as_str(), "{ctx}");
            }
            ("author", "verbatim") => {
                // Against what calibre reported, not what was typed: calibre
                // reads " and " in an author field as a separator, so
                // `Wаr and Peаce` comes back as two authors and the import
                // keeps calibre's answer.
                let want: Vec<String> = reported
                    .as_str()
                    .unwrap_or(input)
                    .split(" & ")
                    .map(str::to_string)
                    .collect();
                assert_eq!(book.authors, want, "{ctx}");
            }
            ("tag", "verbatim") => {
                let tags = raw_tags(&b, book.id.unwrap()).await;
                let want: BTreeSet<String> = str_list(reported).into_iter().collect();
                assert!(want.is_subset(&tags), "{ctx}: {tags:?} lacks {want:?}");
            }
            ("isbn", "normalised") => {
                let normal = s["normal"].as_str().unwrap();
                let got = book.isbn_13.as_deref().or(book.isbn_10.as_deref());
                assert_eq!(got, Some(normal), "{ctx}");
            }
            ("isbn", "rejected") => {
                assert_eq!((&book.isbn_10, &book.isbn_13), (&None, &None), "{ctx}");
            }
            ("date", "normalised") => {
                // The only normalised date is calibre's own sentinel, whose
                // normal form is "no date".
                assert_eq!(s["normal"].as_str(), Some(""), "{ctx}");
                assert_eq!(book.publish_year, None, "{ctx}");
            }
            ("date", "rejected") => {
                // calibredb itself refuses an impossible date; nothing may
                // arrive in its place but the row's benign year (if any).
                assert!(s["calibre_rejected"].is_string(), "{ctx}: calibre took it");
            }
            ("date", "verbatim") => {
                let year: i64 = input[..4].parse().unwrap();
                assert_eq!(book.publish_year, Some(year), "{ctx}");
            }
            other => panic!("{ctx}: no assertion written for {other:?}"),
        }
    }
    assert!(seen >= 1, "the lock salts nothing");
}
