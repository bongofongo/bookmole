//! Data-driven KOReader import harness over `tests/fixtures/koreader/`: golden snapshots,
//! idempotent re-import, superset re-import, and non-fatal malformed input.
//! Regenerate goldens with `UPDATE_GOLDEN=1` or `make golden`.

use std::path::{Path, PathBuf};

use bookmole::koreader::{self, parse_sidecar};
use bookmole::{Book, Diagnostic, DiagnosticKind, Storage};
use serde_json::{Value, json};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/koreader")
}

fn synthetic_dir() -> PathBuf {
    fixtures_root().join("synthetic")
}

fn manifest() -> Value {
    let raw =
        std::fs::read_to_string(fixtures_root().join("manifest.json")).expect("read manifest.json");
    let m: Value = serde_json::from_str(&raw).expect("parse manifest.json");
    m["fixtures"].clone()
}

fn synthetic_fixtures() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(synthetic_dir())
        .expect("read synthetic dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".sdr"))
        .collect();
    names.sort();
    names
}

async fn mem_storage() -> Storage {
    Storage::connect("sqlite::memory:")
        .await
        .expect("open in-memory db")
}

async fn seed_books(storage: &Storage, books: &Value) {
    let Some(arr) = books.as_array() else { return };
    for b in arr {
        let authors = b["authors"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        storage
            .upsert_book(
                &Book {
                    title: b["title"].as_str().map(str::to_owned),
                    authors,
                    // Optional; without an ISBN on the seeded book the sibling-epub match path is never exercised.
                    isbn_10: b["isbn_10"].as_str().map(str::to_owned),
                    isbn_13: b["isbn_13"].as_str().map(str::to_owned),
                    language: b["language"].as_str().map(str::to_owned),
                    ..Default::default()
                },
                None,
            )
            .await
            .expect("seed book");
    }
}

async fn highlight_rows(storage: &Storage, book_id: i64) -> Value {
    let rows = storage
        .list_highlights(book_id)
        .await
        .expect("list highlights");
    Value::Array(
        rows.into_iter()
            .map(|h| {
                json!({
                    "text": h.text,
                    "chapter": h.chapter,
                    "page": h.page,
                    // Keyed `note` after the sidecar's name; `annotation` is absent because import never writes it.
                    "note": h.origin_note,
                    "ko_datetime": h.ko_datetime,
                })
            })
            .collect(),
    )
}

async fn flashcard_words(storage: &Storage, book_id: i64) -> Vec<String> {
    let mut words: Vec<String> = storage
        .list_flashcards_for_book(book_id)
        .await
        .expect("list flashcards")
        .into_iter()
        .map(|f| f.word)
        .collect();
    words.sort();
    words
}

/// Names which degradation fired plus any count, never `Display` (it prints absolute paths).
/// The catch-all panics so a new diagnostic must be taught here on purpose.
fn warning_name(d: &Diagnostic) -> String {
    match &d.kind {
        DiagnosticKind::NoSidecarsFound { .. } => "NoSidecarsFound".to_string(),
        DiagnosticKind::SidecarUnreadable { .. } => "SidecarUnreadable".to_string(),
        DiagnosticKind::SidecarUnparsable { .. } => "SidecarUnparsable".to_string(),
        DiagnosticKind::SidecarNotIdentified { .. } => "SidecarNotIdentified".to_string(),
        DiagnosticKind::UnknownDeviceStatus { .. } => "UnknownDeviceStatus".to_string(),
        DiagnosticKind::SidecarAnchorsUnsupported { entries, .. } => {
            format!("SidecarAnchorsUnsupported({entries})")
        }
        other => {
            panic!("koreader::import produced a diagnostic this harness cannot name: {other:?}")
        }
    }
}

async fn import_to_golden(fixture: &str, books: &Value) -> (Storage, Value) {
    let storage = mem_storage().await;
    seed_books(&storage, books).await;
    let dir = synthetic_dir().join(fixture);
    let report = koreader::import(&storage, &dir, false)
        .await
        .expect("import");

    let mut imported = Vec::new();
    for s in &report.imported {
        imported.push(json!({
            "book_title": s.book_title,
            "inserted": s.inserted,
            "updated": s.updated,
            "skipped": s.skipped,
            "flashcards": s.flashcards,
            // Recorded because the fuzzy title fallback silently masks a broken sibling-epub ISBN match.
            "matched_by": s.matched_by.to_string(),
            "percent_finished": s.percent_finished,
            "status": s.status.as_ref().map(|s| s.to_string()),
            "rating": s.rating,
            "highlights": highlight_rows(&storage, s.book_id).await,
            "flashcard_words": flashcard_words(&storage, s.book_id).await,
        }));
    }
    let unmatched: Vec<Value> = report
        .unmatched
        .iter()
        .map(|u| json!({ "title": u.title }))
        .collect();

    let mut warnings: Vec<String> = report.warnings.iter().map(warning_name).collect();
    warnings.sort();

    let golden = json!({
        "imported": imported,
        "unmatched": unmatched,
        "warnings": warnings,
    });
    (storage, golden)
}

fn golden_path(fixture: &str) -> PathBuf {
    let stem = fixture.strip_suffix(".sdr").unwrap_or(fixture);
    fixtures_root()
        .join("expected")
        .join(format!("{stem}.json"))
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).expect("serialize json")
}

#[tokio::test]
async fn import_matches_golden() {
    let update = std::env::var("UPDATE_GOLDEN").is_ok();
    let man = manifest();
    let mut failures = Vec::new();
    let mut produced: Vec<PathBuf> = Vec::new();

    for fixture in synthetic_fixtures() {
        let books = man
            .get(&fixture)
            .and_then(|f| f.get("books"))
            .cloned()
            .unwrap_or_else(|| json!([]));
        let (_s, actual) = import_to_golden(&fixture, &books).await;
        let path = golden_path(&fixture);

        if update {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("{}\n", pretty(&actual))).unwrap();
            produced.push(path);
            continue;
        }

        let expected_raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => {
                failures.push(format!(
                    "{fixture}: missing golden {} — run `make golden`",
                    path.display()
                ));
                continue;
            }
        };
        let expected: Value = serde_json::from_str(&expected_raw).expect("parse golden");
        if expected != actual {
            failures.push(format!(
                "{fixture}: golden mismatch\n--- expected ---\n{}\n--- actual ---\n{}",
                pretty(&expected),
                pretty(&actual)
            ));
        }
    }

    if update {
        let expected_dir = fixtures_root().join("expected");
        let mut stale = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&expected_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("json") && !produced.contains(&p)
                {
                    stale.push(p);
                }
            }
        }
        stale.sort();
        for p in &stale {
            std::fs::remove_file(p).expect("remove stale golden");
            eprintln!("UPDATE_GOLDEN: removed stale golden {}", p.display());
        }
        eprintln!(
            "UPDATE_GOLDEN: rewrote {} golden snapshots.",
            produced.len()
        );
        return;
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

#[tokio::test]
async fn fixtures_match_by_the_method_the_manifest_expects() {
    let man = manifest();
    let mut checked = 0;

    for fixture in synthetic_fixtures() {
        let Some(want) = man
            .get(&fixture)
            .and_then(|f| f.get("expect"))
            .and_then(|e| e.get("match_via"))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        let books = man
            .get(&fixture)
            .and_then(|f| f.get("books"))
            .cloned()
            .unwrap_or_else(|| json!([]));

        let storage = mem_storage().await;
        seed_books(&storage, &books).await;
        let report = koreader::import(&storage, &synthetic_dir().join(&fixture), false)
            .await
            .expect("import");

        assert_eq!(
            report.imported.len(),
            1,
            "{fixture}: expected exactly one matched book, got {:?}",
            report.imported
        );
        assert_eq!(
            report.imported[0].matched_by.to_string(),
            want,
            "{fixture}: matched by the wrong method"
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "no fixture declares expect.match_via — the ISBN branch is unguarded"
    );
}

/// No fixture can expect `match_via: "md5"` on a first import, so import twice and assert the
/// second pass used the recorded link.
#[tokio::test]
async fn a_second_import_matches_on_the_link_it_recorded() {
    let fixture = "Gen-Stats.sdr";
    let man = manifest();
    let books = man
        .get(fixture)
        .and_then(|f| f.get("books"))
        .cloned()
        .expect("Gen-Stats must be in the manifest");

    let storage = mem_storage().await;
    seed_books(&storage, &books).await;
    let dir = synthetic_dir().join(fixture);

    let first = koreader::import(&storage, &dir, false)
        .await
        .expect("first");
    assert_eq!(
        first.imported[0].matched_by.to_string(),
        "title",
        "nothing is recorded yet, so the first pass can only guess"
    );

    // Mirrors the fixture's root `partial_md5_checksum`; fails if `Gen-Stats` loses that key.
    let linked = storage
        .find_book_by_partial_md5("9f2c4e6a8b0d1357911d3f5b7d9f1a3c")
        .await
        .expect("lookup");
    assert_eq!(
        linked.and_then(|b| b.id),
        Some(first.imported[0].book_id),
        "the import must record the link it just made"
    );

    let second = koreader::import(&storage, &dir, false)
        .await
        .expect("second");
    assert_eq!(second.imported[0].matched_by.to_string(), "md5");
    assert_eq!(second.imported[0].inserted, 0);
}

#[tokio::test]
async fn reimport_is_strictly_idempotent() {
    let man = manifest();
    for fixture in synthetic_fixtures() {
        let books = man
            .get(&fixture)
            .and_then(|f| f.get("books"))
            .cloned()
            .unwrap_or_else(|| json!([]));

        let storage = mem_storage().await;
        seed_books(&storage, &books).await;
        let dir = synthetic_dir().join(&fixture);

        let first = koreader::import(&storage, &dir, false)
            .await
            .expect("first import");
        if first.imported.iter().all(|s| s.inserted == 0) {
            continue;
        }

        let mut before = Vec::new();
        for s in &first.imported {
            before.push((
                s.book_id,
                highlight_rows(&storage, s.book_id).await,
                flashcard_words(&storage, s.book_id).await,
            ));
        }

        let second = koreader::import(&storage, &dir, false)
            .await
            .expect("second import");
        for s in &second.imported {
            assert_eq!(s.inserted, 0, "{fixture}: re-import inserted rows");
            assert_eq!(s.updated, 0, "{fixture}: re-import refreshed rows");
        }
        // Skipped on re-import is `inserted + skipped` of the first run: a sidecar can duplicate its
        // own entries.
        let first_seen: usize = first.imported.iter().map(|s| s.inserted + s.skipped).sum();
        let second_skipped: usize = second.imported.iter().map(|s| s.skipped).sum();
        assert_eq!(
            first_seen, second_skipped,
            "{fixture}: re-import did not skip every entry it saw"
        );

        for (book_id, hl, cards) in before {
            assert_eq!(
                hl,
                highlight_rows(&storage, book_id).await,
                "{fixture}: highlights changed"
            );
            assert_eq!(
                cards,
                flashcard_words(&storage, book_id).await,
                "{fixture}: flashcards changed"
            );
        }
    }
}

#[tokio::test]
async fn appends_only_new_on_partial_reimport() {
    let storage = mem_storage().await;
    seed_books(
        &storage,
        &json!([{ "title": "Middlemarch", "authors": ["George Eliot"] }]),
    )
    .await;

    let base = synthetic_dir().join("Middlemarch.sdr");
    let first = koreader::import(&storage, &base, false)
        .await
        .expect("base import");
    let book_id = first.imported[0].book_id;
    let base_inserted = first.imported[0].inserted;
    let before = highlight_rows(&storage, book_id).await;

    let superset_dir = synthetic_dir().join("variants/Middlemarch-Superset.sdr");
    let second = koreader::import(&storage, &superset_dir, false)
        .await
        .expect("superset import");

    let s = &second.imported[0];
    assert_eq!(s.inserted, 1, "exactly one new highlight expected");
    assert_eq!(s.skipped, base_inserted, "all originals should be skipped");

    let after = highlight_rows(&storage, book_id).await;
    let before_arr = before.as_array().unwrap();
    let after_arr = after.as_array().unwrap();
    assert_eq!(after_arr.len(), before_arr.len() + 1);
    for row in before_arr {
        assert!(
            after_arr.contains(row),
            "an original highlight went missing"
        );
    }
    assert!(
        after_arr
            .iter()
            .any(|r| r["text"] == "A brand new highlight added later."),
        "new highlight not stored"
    );
}

#[tokio::test]
async fn device_edits_refresh_but_ours_survive() {
    const EDITED: &str = "We are all of us born in moral stupidity.";

    let storage = mem_storage().await;
    seed_books(
        &storage,
        &json!([{ "title": "Middlemarch", "authors": ["George Eliot"] }]),
    )
    .await;

    let first = koreader::import(&storage, &synthetic_dir().join("Middlemarch.sdr"), false)
        .await
        .expect("base import");
    let book_id = first.imported[0].book_id;
    assert_eq!(first.imported[0].updated, 0, "nothing to refresh yet");

    let before: Vec<_> = storage.list_highlights(book_id).await.expect("list");
    let target = before
        .iter()
        .find(|h| h.text == EDITED)
        .expect("the highlight the variant edits");
    storage
        .set_annotation(target.id, Some("mine, and not the device's business"))
        .await
        .expect("annotate");

    let second = koreader::import(
        &storage,
        &synthetic_dir().join("variants/Middlemarch-NoteEdited.sdr"),
        false,
    )
    .await
    .expect("edited import");

    let s = &second.imported[0];
    assert_eq!(s.inserted, 0, "an edited note is not a new highlight");
    assert_eq!(s.updated, 1, "exactly the one edited row");
    assert_eq!(s.skipped, before.len() - 1, "the rest are untouched");

    let after = storage.list_highlights(book_id).await.expect("list");
    assert_eq!(after.len(), before.len(), "no row was added or lost");

    let refreshed = after
        .iter()
        .find(|h| h.text == EDITED)
        .expect("still there");
    assert_eq!(
        refreshed.origin_note.as_deref(),
        Some("Rewritten on the device weeks later.")
    );
    assert_eq!(
        refreshed.annotation.as_deref(),
        Some("mine, and not the device's business")
    );
    assert_eq!(refreshed.source, target.source, "provenance is not payload");
    assert_eq!(
        refreshed.created_at, target.created_at,
        "created_at is when WE stored it; a refresh must not restamp it"
    );
    assert_eq!(refreshed.text, target.text, "highlight text is frozen");
}

/// `notes.highlight_id` and `flashcards.highlight_id` are foreign keys, so a delete-and-reinsert
/// refresh would null note anchors and cascade flashcards away.
#[tokio::test]
async fn highlight_ids_are_stable_across_refresh() {
    let storage = mem_storage().await;
    seed_books(
        &storage,
        &json!([{ "title": "Middlemarch", "authors": ["George Eliot"] }]),
    )
    .await;

    let first = koreader::import(&storage, &synthetic_dir().join("Middlemarch.sdr"), false)
        .await
        .expect("base import");
    let book_id = first.imported[0].book_id;

    let ids_before: Vec<i64> = storage
        .list_highlights(book_id)
        .await
        .expect("list")
        .iter()
        .map(|h| h.id)
        .collect();
    let cards_before = flashcard_words(&storage, book_id).await;
    assert!(!ids_before.is_empty(), "nothing to keep stable");
    assert!(
        !cards_before.is_empty(),
        "the FK that would cascade is unused"
    );

    koreader::import(
        &storage,
        &synthetic_dir().join("variants/Middlemarch-NoteEdited.sdr"),
        false,
    )
    .await
    .expect("edited import");

    let ids_after: Vec<i64> = storage
        .list_highlights(book_id)
        .await
        .expect("list")
        .iter()
        .map(|h| h.id)
        .collect();
    assert_eq!(ids_before, ids_after, "a refresh must update in place");
    assert_eq!(
        cards_before,
        flashcard_words(&storage, book_id).await,
        "flashcards hang off highlight_id and would have cascaded away"
    );
}

#[tokio::test]
async fn pdf_anchors_are_reported_rather_than_silently_dropped() {
    let fixture = "Gen-Pdf-Anchors.sdr";
    let man = manifest();
    let books = man
        .get(fixture)
        .and_then(|f| f.get("books"))
        .cloned()
        .expect("Gen-Pdf-Anchors must be in the manifest");

    let storage = mem_storage().await;
    seed_books(&storage, &books).await;
    let report = koreader::import(&storage, &synthetic_dir().join(fixture), false)
        .await
        .expect("a PDF sidecar must not abort an import");

    assert_eq!(report.imported.len(), 1, "the sidecar must still match");
    assert_eq!(report.imported[0].inserted, 0);
    assert!(report.unmatched.is_empty());

    let anchors: Vec<usize> = report
        .warnings
        .iter()
        .filter_map(|d| match d.kind {
            DiagnosticKind::SidecarAnchorsUnsupported { entries, .. } => Some(entries),
            _ => None,
        })
        .collect();
    assert_eq!(
        anchors,
        vec![3],
        "expected exactly one diagnostic naming three lost highlights, got {:?}",
        report.warnings
    );

    // One diagnostic per file, not per entry: a per-highlight diagnostic would be noise.
    assert_eq!(
        report.warnings.len(),
        1,
        "a 300-highlight PDF must not emit 300 diagnostics"
    );

    let text = report.warnings[0].to_string();
    assert!(text.contains("metadata.pdf.lua"), "no path in {text:?}");
    assert!(text.contains('3'), "no count in {text:?}");
}

#[tokio::test]
async fn only_the_paging_fixture_reports_an_unsupported_anchor() {
    let storage = mem_storage().await;
    seed_all_synthetic(&storage).await;
    let report = koreader::import(&storage, &synthetic_dir(), false)
        .await
        .expect("bulk import");

    let named: Vec<String> = report
        .warnings
        .iter()
        .filter_map(|d| match &d.kind {
            DiagnosticKind::SidecarAnchorsUnsupported { path, entries } => {
                Some(format!("{}:{entries}", path.display()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(named.len(), 1, "expected exactly one, got {named:?}");
    assert!(
        named[0].ends_with("Gen-Pdf-Anchors.sdr/metadata.pdf.lua:3"),
        "the wrong fixture reported an unsupported anchor: {named:?}"
    );
}

#[tokio::test]
async fn malformed_and_unmatched_are_non_fatal() {
    let storage = mem_storage().await;
    let report = koreader::import(&storage, &synthetic_dir().join("Malformed.sdr"), false)
        .await
        .expect("import must not error on bad lua");
    assert!(report.imported.is_empty());
    assert!(report.unmatched.is_empty());
    assert!(!report.warnings.is_empty(), "malformed sidecar should warn");

    let storage = mem_storage().await;
    let report = koreader::import(&storage, &synthetic_dir().join("Unmatched.sdr"), false)
        .await
        .expect("import");
    assert!(report.imported.is_empty());
    assert_eq!(report.unmatched.len(), 1);
    assert_eq!(
        report.unmatched[0].title.as_deref(),
        Some("Nonexistent Tome")
    );

    let storage = mem_storage().await;
    seed_all_synthetic(&storage).await;
    let report = koreader::import(&storage, &synthetic_dir(), false)
        .await
        .expect("bulk import");
    assert!(!report.imported.is_empty(), "bulk import found no books");
    assert!(
        !report.warnings.is_empty(),
        "malformed fixture should still warn in bulk"
    );
}

async fn seed_all_synthetic(storage: &Storage) {
    let man = manifest();
    if let Some(obj) = man.as_object() {
        for f in obj.values() {
            if let Some(books) = f.get("books") {
                seed_books(storage, books).await;
            }
        }
    }
}

#[tokio::test]
async fn real_exports_are_idempotent() {
    let real = fixtures_root().join("real");
    let sidecars = koreader::find_sidecars(&real).expect("scan real dir");
    if sidecars.is_empty() {
        if std::env::var("BOOKMOLE_REQUIRE_FIXTURES").is_ok() {
            panic!(
                "REQUIRE_FIXTURES set but no drop-in exports under {}",
                real.display()
            );
        }
        eprintln!(
            "SKIPPED real_exports_are_idempotent: no drop-in exports under {}",
            real.display()
        );
        return;
    }

    let storage = mem_storage().await;
    for path in &sidecars {
        if let Ok(src) = std::fs::read_to_string(path)
            && let Ok(sc) = parse_sidecar(&src)
            && let Some(title) = sc.title
        {
            storage
                .upsert_book(
                    &Book {
                        title: Some(title),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .expect("seed real book");
        }
    }

    let first = koreader::import(&storage, &real, false)
        .await
        .expect("first real import");
    let mut before = Vec::new();
    for s in &first.imported {
        before.push((
            s.book_id,
            highlight_rows(&storage, s.book_id).await,
            flashcard_words(&storage, s.book_id).await,
        ));
    }

    let second = koreader::import(&storage, &real, false)
        .await
        .expect("second real import");
    for s in &second.imported {
        assert_eq!(
            s.inserted, 0,
            "real re-import inserted new rows for {}",
            s.book_title
        );
    }
    for (book_id, hl, cards) in before {
        assert_eq!(
            hl,
            highlight_rows(&storage, book_id).await,
            "real highlights changed on re-import"
        );
        assert_eq!(
            cards,
            flashcard_words(&storage, book_id).await,
            "real flashcards changed on re-import"
        );
    }
    eprintln!(
        "real_exports_are_idempotent: verified {} sidecar(s), {} book(s) imported",
        sidecars.len(),
        first.imported.len()
    );
}

#[tokio::test]
async fn a_nested_library_tree_is_walked_to_its_leaves() {
    let tmp = tempfile::tempdir().unwrap();
    let deep = tmp
        .path()
        .join("Fiction/Translated/Japanese/Modern/The Trial.sdr");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(
        deep.join("metadata.epub.lua"),
        std::fs::read(
            synthetic_dir()
                .join("The-Trial.sdr")
                .join("metadata.epub.lua"),
        )
        .unwrap(),
    )
    .unwrap();
    std::fs::write(deep.join("cover.jpg"), b"not a sidecar").unwrap();
    std::fs::write(deep.join("notes.txt"), b"also not a sidecar").unwrap();

    let found = koreader::find_sidecars(tmp.path()).expect("walk");
    assert_eq!(found.len(), 1, "expected one sidecar, got {found:?}");

    let storage = mem_storage().await;
    seed_books(
        &storage,
        &json!([{ "title": "The Trial", "authors": ["Franz Kafka"] }]),
    )
    .await;
    let report = koreader::import(&storage, tmp.path(), false)
        .await
        .expect("import from a nested tree");
    assert_eq!(report.imported.len(), 1);
    assert!(report.imported[0].inserted > 0);
}

/// Not a CI case: `cargo test -p bookmole -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "scale check; run explicitly"]
async fn a_very_large_export_imports_in_reasonable_time() {
    const N: usize = 5_000;

    let mut lua = String::from("return {\n    [\"annotations\"] = {\n");
    for i in 1..=N {
        lua.push_str(&format!(
            "        [{i}] = {{ [\"text\"] = \"highlight number {i}\", \
             [\"pos0\"] = \"/body/p[{i}]/text().0\", [\"pageno\"] = {i}, \
             [\"datetime\"] = \"2026-01-01 00:00:00\" }},\n"
        ));
    }
    lua.push_str("    },\n    [\"doc_props\"] = { [\"title\"] = \"Enormous Book\" },\n}\n");

    let tmp = tempfile::tempdir().unwrap();
    let sdr = tmp.path().join("Enormous Book.sdr");
    std::fs::create_dir_all(&sdr).unwrap();
    std::fs::write(sdr.join("metadata.epub.lua"), &lua).unwrap();

    let storage = mem_storage().await;
    seed_books(
        &storage,
        &json!([{ "title": "Enormous Book", "authors": [] }]),
    )
    .await;

    let started = std::time::Instant::now();
    let report = koreader::import(&storage, tmp.path(), false)
        .await
        .expect("large import");
    let elapsed = started.elapsed();

    assert_eq!(report.imported[0].inserted, N);
    eprintln!(
        "imported {N} highlights in {elapsed:?} ({:.0}/s)",
        N as f64 / elapsed.as_secs_f64()
    );
}

async fn middlemarch_md5() -> String {
    let src = std::fs::read_to_string(synthetic_dir().join("Middlemarch.sdr/metadata.epub.lua"))
        .expect("read sidecar");
    parse_sidecar(&src)
        .expect("parse")
        .partial_md5
        .expect("the fixture has a partial_md5")
}

#[tokio::test]
async fn import_stamps_origin_and_links_the_reader_copy() {
    use bookmole::copies::{LearnedFrom, PlaceKind};
    use bookmole::{HighlightOrigin, PosKind};

    let storage = mem_storage().await;
    seed_books(
        &storage,
        &json!([{ "title": "Middlemarch", "authors": ["George Eliot"] }]),
    )
    .await;
    let md5 = middlemarch_md5().await;

    let first = koreader::import(&storage, &synthetic_dir().join("Middlemarch.sdr"), false)
        .await
        .expect("import");
    let book_id = first.imported[0].book_id;

    let copies = storage.copies_for_book(book_id).await.expect("copies");
    assert_eq!(copies.len(), 1, "one file, one copy");
    assert_eq!(copies[0].learned_from, LearnedFrom::Koreader);
    assert_eq!(copies[0].have_now, None, "import never decides have_now");
    assert_eq!(copies[0].places.len(), 1);
    assert_eq!(copies[0].places[0].kind, PlaceKind::Reader);
    assert_eq!(copies[0].places[0].reference, md5);

    let highlights = storage.list_highlights(book_id).await.expect("list");
    assert!(!highlights.is_empty());
    for h in &highlights {
        assert_eq!(h.origin, HighlightOrigin::Koreader);
        assert_eq!(h.copy_id, Some(copies[0].id));
        assert_eq!(h.origin_id, None, "KOReader keeps using identity_hash");
        assert_eq!(h.pos_kind, Some(PosKind::KoreaderXpointer));
        assert!(!h.text_unavailable);
    }

    let second = koreader::import(&storage, &synthetic_dir().join("Middlemarch.sdr"), false)
        .await
        .expect("re-import");
    assert_eq!(second.imported[0].inserted, 0);
    assert_eq!(second.imported[0].updated, 0);
    let again = storage.copies_for_book(book_id).await.expect("copies");
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].places.len(), 1);
}

#[tokio::test]
async fn import_attaches_to_the_books_existing_file_copy() {
    use bookmole::copies::{CopyForm, LearnedFrom, NewCopy, PlaceKind, Tenure};

    let storage = mem_storage().await;
    seed_books(
        &storage,
        &json!([{ "title": "Middlemarch", "authors": ["George Eliot"] }]),
    )
    .await;
    let first = koreader::import(&storage, &synthetic_dir().join("Middlemarch.sdr"), true)
        .await
        .expect("dry run");
    let book_id = first.imported[0].book_id;
    let calibres = storage
        .create_copy(&NewCopy::new(
            book_id,
            Some(CopyForm::File),
            Tenure::Own,
            LearnedFrom::Calibre,
        ))
        .await
        .expect("copy");

    koreader::import(&storage, &synthetic_dir().join("Middlemarch.sdr"), false)
        .await
        .expect("import");
    let copies = storage.copies_for_book(book_id).await.expect("copies");
    assert_eq!(copies.len(), 1, "no second copy of the same file");
    assert_eq!(copies[0].id, calibres.id);
    assert_eq!(copies[0].learned_from, LearnedFrom::Calibre);
    assert!(copies[0].places.iter().any(|p| p.kind == PlaceKind::Reader));

    storage.delete_copy(calibres.id).await.expect("delete");
    let orphaned = storage.list_highlights(book_id).await.expect("list");
    assert!(orphaned.iter().all(|h| h.copy_id.is_none()));
    let again = koreader::import(&storage, &synthetic_dir().join("Middlemarch.sdr"), false)
        .await
        .expect("re-import");
    assert_eq!(again.imported[0].updated, 0);
    let copies = storage.copies_for_book(book_id).await.expect("copies");
    assert_eq!(copies.len(), 1);
    assert!(
        storage
            .list_highlights(book_id)
            .await
            .expect("list")
            .iter()
            .all(|h| h.copy_id == Some(copies[0].id))
    );
}

#[tokio::test]
async fn the_same_passage_from_two_origins_is_two_rows_in_one_group() {
    use bookmole::{HighlightOrigin, NewHighlight, same_passages};

    let storage = mem_storage().await;
    seed_books(
        &storage,
        &json!([{ "title": "Middlemarch", "authors": ["George Eliot"] }]),
    )
    .await;
    let book_id = storage
        .upsert_book(
            &Book {
                title: Some("Grouping".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("book");
    let make = |text: &str, when: &str, source: &str| NewHighlight {
        text: text.into(),
        chapter: None,
        page: None,
        pos0: None,
        pos1: None,
        ko_datetime: Some(when.into()),
        ko_datetime_updated: None,
        color: None,
        note: None,
        source: source.into(),
    };
    let a = storage
        .insert_highlight(
            book_id,
            &make("It\u{2019}s  a Long \u{2014} road.", "t1", "koreader"),
        )
        .await
        .unwrap()
        .unwrap();
    let b = storage
        .insert_highlight(book_id, &make("it's a long - road.", "t2", "user"))
        .await
        .unwrap()
        .unwrap();
    let lone = storage
        .insert_highlight(book_id, &make("Something else entirely.", "t3", "user"))
        .await
        .unwrap()
        .unwrap();

    let all = storage.list_highlights(book_id).await.unwrap();
    assert_eq!(all.len(), 3, "never merged");
    assert_eq!(same_passages(&all), vec![vec![a, b]]);
    let origin = |id: i64| all.iter().find(|h| h.id == id).unwrap().origin;
    assert_eq!(origin(a), HighlightOrigin::Koreader);
    assert_eq!(origin(b), HighlightOrigin::User);
    assert_eq!(origin(lone), HighlightOrigin::User);
}

#[tokio::test]
async fn import_records_the_paired_device_on_the_reader_place() {
    use bookmole::copies::PlaceKind;

    let storage = mem_storage().await;
    seed_books(
        &storage,
        &json!([{ "title": "Middlemarch", "authors": ["George Eliot"] }]),
    )
    .await;
    let md5 = middlemarch_md5().await;
    let sdr = synthetic_dir().join("Middlemarch.sdr");

    let first = koreader::import(&storage, &sdr, false)
        .await
        .expect("import");
    let book_id = first.imported[0].book_id;
    let place = |copies: Vec<bookmole::Copy>| copies[0].places[0].clone();
    let copies = storage.copies_for_book(book_id).await.unwrap();
    assert_eq!(place(copies).detail, None);

    koreader::import_for_device(&storage, &sdr, false, Some("kobo-1"))
        .await
        .expect("import for device");
    let copies = storage.copies_for_book(book_id).await.unwrap();
    assert_eq!(copies.len(), 1);
    let p = place(copies);
    assert_eq!(
        (p.kind, p.reference.as_str()),
        (PlaceKind::Reader, md5.as_str())
    );
    assert_eq!(p.detail.as_deref(), Some("kobo-1"));

    koreader::import(&storage, &sdr, false)
        .await
        .expect("plain");
    koreader::import_for_device(&storage, &sdr, false, Some("kobo-2"))
        .await
        .expect("other device");
    let copies = storage.copies_for_book(book_id).await.unwrap();
    assert_eq!(place(copies).detail.as_deref(), Some("kobo-1"));
}

async fn file_storage(dir: &Path) -> (Storage, sqlx::SqlitePool) {
    let url = format!("sqlite://{}?mode=rwc", dir.join("app.db").display());
    let storage = Storage::connect(&url).await.expect("open db");
    let pool = sqlx::SqlitePool::connect(&url)
        .await
        .expect("second handle");
    (storage, pool)
}

async fn stored_hashes(pool: &sqlx::SqlitePool) -> Vec<String> {
    sqlx::query_scalar("SELECT identity_hash FROM highlights ORDER BY identity_hash")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn one_sidecar_hashes_alike_in_two_libraries() {
    let fixture = "Middlemarch.sdr";
    let books = manifest()[fixture]["books"].clone();
    let dir = synthetic_dir().join(fixture);

    let (tmp_a, tmp_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, pool_a) = file_storage(tmp_a.path()).await;
    let (b, pool_b) = file_storage(tmp_b.path()).await;
    seed_books(&a, &books).await;
    // Other books first, so the same book has another local id in the second library.
    seed_books(
        &b,
        &json!([{ "title": "Silas Marner" }, { "title": "Adam Bede" }]),
    )
    .await;
    seed_books(&b, &books).await;

    let in_a = koreader::import(&a, &dir, false).await.expect("import a");
    let in_b = koreader::import(&b, &dir, false).await.expect("import b");
    assert_ne!(in_a.imported[0].book_id, in_b.imported[0].book_id);
    assert!(in_a.imported[0].inserted > 0);

    let hashes = stored_hashes(&pool_a).await;
    assert_eq!(hashes.len(), in_a.imported[0].inserted);
    assert_eq!(hashes, stored_hashes(&pool_b).await);
}

#[tokio::test]
async fn a_reimport_after_a_merge_adds_nothing() {
    let fixture = "Gen-Stats.sdr";
    let books = manifest()[fixture]["books"].clone();
    let dir = synthetic_dir().join(fixture);
    let tmp = tempfile::tempdir().unwrap();
    let (storage, pool) = file_storage(tmp.path()).await;
    seed_books(&storage, &books).await;

    let first = koreader::import(&storage, &dir, false)
        .await
        .expect("first");
    let src = first.imported[0].book_id;
    assert!(first.imported[0].inserted > 0);
    let before = stored_hashes(&pool).await;

    let dst = storage
        .upsert_book(
            &Book {
                title: Some("A Counted Book (another record)".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    storage.merge_books(src, dst).await.unwrap();
    assert_eq!(
        stored_hashes(&pool).await,
        before,
        "a merge moves highlights without rehashing them"
    );

    let again = koreader::import(&storage, &dir, false)
        .await
        .expect("again");
    assert_eq!(again.imported[0].book_id, dst, "the device link moved too");
    assert_eq!(again.imported[0].inserted, 0);
    assert_eq!(
        storage.list_highlights(dst).await.unwrap().len(),
        before.len()
    );
}
