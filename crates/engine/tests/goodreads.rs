//! Goodreads CSV import and export through the facade; `recorded/` fixtures are hand-authored
//! from real exports, `generated/` ones cover volume.

mod common;

use std::path::{Path, PathBuf};

use bookmole::goodreads::{ImportOptions, Shelf, parse_csv};
use bookmole::{Book, DiagnosticKind, Engine, GoodreadsMatch, NoteScope, TextOutcome};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/goodreads");

fn recorded(name: &str) -> PathBuf {
    Path::new(FIXTURES).join("recorded").join(name)
}

fn generated() -> PathBuf {
    Path::new(FIXTURES).join("generated/library.csv")
}

fn plain() -> ImportOptions {
    ImportOptions::default()
}

/// Format tests use `create_ambiguous` so a row in the candidate band cannot shrink the library.
fn creating() -> ImportOptions {
    ImportOptions {
        create_ambiguous: true,
        ..Default::default()
    }
}

fn tmp_csv(dir: &Path, name: &str, csv: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, csv).expect("write csv");
    path
}

#[test]
fn the_recorded_export_parses_the_shapes_only_a_real_file_has() {
    let rows = parse_csv(&recorded("library-export.csv")).expect("parses");
    assert_eq!(rows.len(), 5);

    assert_eq!(rows[0].isbn_13.as_deref(), Some("9781900000178"));
    assert_eq!(rows[0].isbn_10.as_deref(), Some("1900000172"));
    assert_eq!(rows[0].external_id.as_deref(), Some("9000001"));
    assert_eq!(rows[0].shelves, vec!["victorian", "favourites"]);
    assert_eq!(rows[0].exclusive_shelf, Some(Shelf::Read));

    assert_eq!(rows[1].read_count, 3);
    assert!(rows[1].date_read.is_some());
    assert_eq!(rows[1].authors.len(), 2);

    assert_eq!(rows[2].isbn_10, None);
    assert_eq!(rows[2].isbn_13, None);
    assert_eq!(rows[2].exclusive_shelf, Some(Shelf::CurrentlyReading));

    assert_eq!(rows[2].my_rating, None, "an empty cell said nothing");
    assert_eq!(rows[3].my_rating, Some(0.0), "a literal 0 said 'unrated'");

    assert_eq!(
        rows[4].title.as_deref(),
        Some(r#"Frankenstein, or "The Modern Prometheus""#)
    );
    assert!(rows[4].review.as_deref().unwrap().contains('\n'));
    assert_eq!(
        rows[4].private_notes.as_deref(),
        Some("Library copy; return by Friday.")
    );
    assert_eq!(rows[4].read_count, 1);
}

#[test]
fn the_importers_smaller_column_set_reads_too() {
    let rows = parse_csv(&recorded("importer-columns.csv")).expect("parses");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].my_rating, Some(5.0));
    assert_eq!(
        rows[0].review.as_deref(),
        Some("A whole town seen at once.")
    );
    assert_eq!(rows[0].isbn_13.as_deref(), Some("9781900000178"));
    assert_eq!(rows[0].exclusive_shelf, None);
    assert_eq!(rows[1].date_read, None);
}

#[tokio::test]
async fn a_file_that_is_not_a_goodreads_export_says_so() {
    let (tmp, engine) = common::engine().await;
    let path = tmp_csv(tmp.path(), "nope.csv", "a,b,c\n1,2,3\n");
    let err = engine
        .import_goodreads(&path, plain())
        .await
        .expect_err("no Title column");
    assert!(err.to_string().contains("Title"), "{err}");
}

#[tokio::test]
async fn the_recorded_export_lands_where_it_should() {
    let (_tmp, engine) = common::engine().await;
    let report = engine
        .import_goodreads(&recorded("library-export.csv"), plain())
        .await
        .expect("imports");

    assert_eq!(report.rows, 5);
    assert_eq!(report.books.len(), 5);
    assert_eq!(report.created(), 5, "an empty library creates every row");
    assert!(report.unmatched.is_empty());

    let middlemarch = book(&engine, "Middlemarch").await;
    let readings = engine
        .storage()
        .list_readings(middlemarch.id.unwrap())
        .await
        .unwrap();
    assert_eq!(readings.len(), 1);
    assert!(readings[0].finished_at.is_some());
    assert_eq!(readings[0].source, "goodreads");
    assert_eq!(readings[0].status, "finished");
    assert_eq!(report.books[0].rating, Some(5.0));
    assert_eq!(report.books[0].review, TextOutcome::Written);

    let claimed = engine
        .storage()
        .field_provenance(middlemarch.id.unwrap())
        .await
        .unwrap();
    assert!(
        claimed.iter().all(|f| f.source == "goodreads"),
        "this book has one origin so far: {claimed:?}"
    );
    let fields: Vec<&str> = claimed.iter().map(|f| f.field.as_str()).collect();
    assert!(fields.contains(&"title") && fields.contains(&"page_count"));
    assert!(!fields.contains(&"description"), "{fields:?}");

    let crime = book(&engine, "Crime and Punishment").await;
    let readings = engine
        .storage()
        .list_readings(crime.id.unwrap())
        .await
        .unwrap();
    assert_eq!(readings.len(), 3, "Read Count 3 means three readings");
    assert!(
        readings.iter().all(|r| r.started_at.is_none()),
        "Goodreads' CSV has no start date, and inventing one is the thing this refuses"
    );
    assert_eq!(report.books[1].rating, Some(4.0));

    let kokoro = book(&engine, "Kokoro").await;
    let readings = engine
        .storage()
        .list_readings(kokoro.id.unwrap())
        .await
        .unwrap();
    assert_eq!(readings.len(), 1);
    assert_eq!(readings[0].finished_at, None, "open");
    assert_eq!(readings[0].status, "reading");

    let left = book(&engine, "The Moonstone").await;
    assert!(
        engine
            .storage()
            .list_readings(left.id.unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(report.books[3].rating, None);

    let tags = engine.storage().book_tags(left.id.unwrap()).await.unwrap();
    assert_eq!(
        tags.iter().map(|t| t.tag.as_str()).collect::<Vec<_>>(),
        vec!["mystery", "to-read"]
    );
    assert!(tags.iter().all(|t| t.source == "goodreads"));

    // `Private Notes` become a note of their own, never folded into a review or reflection.
    let frankenstein = book(&engine, "Frankenstein").await;
    let notes = engine
        .list_notes(NoteScope::of_book(frankenstein.id), None)
        .await
        .unwrap();
    assert!(
        notes
            .iter()
            .any(|n| n.title.starts_with("Goodreads notes:") && n.kind == "note"),
        "{notes:?}"
    );
    assert!(notes.iter().any(|n| n.kind == "review"));
}

#[tokio::test]
async fn to_read_is_not_in_the_library_and_read_gets_an_unrecorded_copy() {
    let (_tmp, engine) = common::engine().await;
    engine
        .import_goodreads(&recorded("library-export.csv"), plain())
        .await
        .expect("imports");

    let want = book(&engine, "The Moonstone").await;
    let id = want.id.unwrap();
    assert!(engine.copies_for_book(id).await.unwrap().is_empty());
    let standing = engine.book_standing(id).await.unwrap();
    assert!(!standing.in_library);
    assert!(!standing.read_without_copy);
    assert!(
        engine
            .storage()
            .book_tags(id)
            .await
            .unwrap()
            .iter()
            .any(|t| t.source == "goodreads" && t.tag == "to-read")
    );

    for title in ["Middlemarch", "Crime and Punishment", "Kokoro"] {
        let id = book(&engine, title).await.id.unwrap();
        let copies = engine.copies_for_book(id).await.unwrap();
        assert_eq!(copies.len(), 1, "{title}");
        let c = &copies[0];
        assert_eq!(c.form, None);
        assert_eq!(c.tenure, bookmole::Tenure::Unrecorded);
        assert_eq!(c.have_now, None);
        assert_eq!(c.learned_from, bookmole::LearnedFrom::User);
        assert!(c.places.is_empty());
        let standing = engine.book_standing(id).await.unwrap();
        assert!(standing.in_library, "{title}");
        assert!(!standing.read_without_copy, "{title}");
    }

    engine
        .import_goodreads(&recorded("library-export.csv"), plain())
        .await
        .expect("imports");
    let id = book(&engine, "Middlemarch").await.id.unwrap();
    assert_eq!(engine.copies_for_book(id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn re_importing_the_same_export_changes_nothing() {
    let (_tmp, engine) = common::engine().await;
    let path = recorded("library-export.csv");
    engine.import_goodreads(&path, plain()).await.unwrap();
    let before = snapshot(&engine).await;

    let again = engine.import_goodreads(&path, plain()).await.unwrap();
    assert_eq!(again.created(), 0, "no second copy of anything");
    assert!(again.books.iter().all(|b| b.readings_added == 0));
    assert!(again.books.iter().all(|b| b.tags_added == 0));
    assert!(
        again.books.iter().all(|b| b.rating.is_none()),
        "a rating already stored identically is not a change"
    );
    assert!(
        again
            .books
            .iter()
            .all(|b| b.review != TextOutcome::KeptOurs),
        "an unedited review is Unchanged, not a conflict"
    );
    assert_eq!(snapshot(&engine).await, before);
}

#[tokio::test]
async fn an_isbn_finds_the_book_already_on_the_shelf() {
    let (_tmp, engine) = common::engine().await;
    let existing = engine
        .save_book(&Book {
            title: Some("Bluets".into()),
            isbn_13: Some("9781900000178".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let report = engine
        .import_goodreads(&recorded("library-export.csv"), plain())
        .await
        .unwrap();
    assert_eq!(report.books[0].matched_by, GoodreadsMatch::Isbn);
    assert_eq!(report.books[0].book_id, existing.id);
    assert_eq!(report.created(), 4, "the other four are new");
}

#[tokio::test]
async fn a_near_miss_is_reported_with_candidates_rather_than_duplicated() {
    let (_tmp, engine) = common::engine().await;
    // A subtitle the library carries and the CSV lacks: a real variant of the same work.
    let near = engine
        .save_book(&Book {
            title: Some("Middlemarch: A Study of Provincial Life".into()),
            // Stored in the origin's "Last, First" spelling.
            authors: vec!["Eliot, George".into()],
            publish_year: Some(1872),
            ..Default::default()
        })
        .await
        .unwrap();

    let path = recorded("importer-columns.csv");
    let report = engine.import_goodreads(&path, plain()).await.unwrap();
    let unmatched = report
        .unmatched
        .iter()
        .find(|u| u.title == "Middlemarch")
        .expect("Middlemarch is in the band, not over it");
    assert_eq!(unmatched.candidates.len(), 1);
    assert_eq!(unmatched.candidates[0].book_id, near.id.unwrap());
    assert_eq!(unmatched.candidates[0].authors_display, ["George Eliot"]);
    assert_eq!(unmatched.candidates[0].publish_year, Some(1872));
    assert!(
        (0.60..0.85).contains(&unmatched.candidates[0].score),
        "the fixture has drifted out of the candidate band: {}",
        unmatched.candidates[0].score
    );
    let on_the_shelf = engine.resolve_books("Middlemarch").await.unwrap();
    assert_eq!(
        on_the_shelf.iter().map(|b| b.id).collect::<Vec<_>>(),
        [near.id],
        "nothing was created behind the user's back — only the book already there"
    );

    let forced = engine.import_goodreads(&path, creating()).await.unwrap();
    assert!(forced.unmatched.is_empty());
    assert_eq!(forced.created(), 1);
}

#[tokio::test]
async fn a_dry_run_writes_nothing() {
    let (_tmp, engine) = common::engine().await;
    let path = recorded("library-export.csv");
    let preview = engine
        .import_goodreads(
            &path,
            ImportOptions {
                dry_run: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert!(preview.dry_run);
    assert_eq!(preview.books.len(), 5);
    assert!(preview.books.iter().all(|b| b.book_id.is_none()));
    assert_eq!(
        preview.books[1].readings_added, 3,
        "the preview counts what the import would write"
    );
    assert!(
        engine
            .storage()
            .list_books(&bookmole::BookQuery::new(50, bookmole::BookSort::Title))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .list_notes(NoteScope::All, None)
            .await
            .unwrap()
            .is_empty()
    );

    let real = engine.import_goodreads(&path, plain()).await.unwrap();
    for (p, r) in preview.books.iter().zip(&real.books) {
        assert_eq!(p.title, r.title);
        assert_eq!(p.readings_added, r.readings_added, "{}", p.title);
        assert_eq!(p.tags_added, r.tags_added, "{}", p.title);
        assert_eq!(p.rating, r.rating, "{}", p.title);
    }
}

#[tokio::test]
async fn a_review_edited_here_survives_a_re_import() {
    let (_tmp, engine) = common::engine().await;
    let path = recorded("library-export.csv");
    engine.import_goodreads(&path, plain()).await.unwrap();

    let middlemarch = book(&engine, "Middlemarch").await;
    let review = engine
        .list_notes(NoteScope::of_book(middlemarch.id), None)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.kind == "review")
        .expect("imported review");
    engine
        .update_note_body(&review, "Rewritten here, months later.")
        .await
        .unwrap();

    let again = engine.import_goodreads(&path, plain()).await.unwrap();
    assert_eq!(again.books[0].review, TextOutcome::KeptOurs);
    assert!(
        again
            .warnings
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::GoodreadsReviewDiverged { .. })),
        "silence would look exactly like success"
    );
    assert_eq!(
        engine.note_body(&review).unwrap(),
        "Rewritten here, months later."
    );
}

#[tokio::test]
async fn a_rating_on_our_own_scale_is_not_overwritten() {
    let (_tmp, engine) = common::engine().await;
    let path = recorded("library-export.csv");
    engine.import_goodreads(&path, plain()).await.unwrap();

    let middlemarch = book(&engine, "Middlemarch").await;
    let review = engine
        .list_notes(NoteScope::of_book(middlemarch.id), None)
        .await
        .unwrap()
        .into_iter()
        .find(|n| n.kind == "review")
        .unwrap();
    engine.set_rating(review.id, 4.5).await.unwrap();

    let again = engine.import_goodreads(&path, plain()).await.unwrap();
    assert_eq!(again.books[0].rating, None);
    assert!(
        again
            .warnings
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::GoodreadsRatingDiverged { .. }))
    );
    let kept = engine
        .storage()
        .review_rating(review.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.value, 4.5);
    assert_eq!(kept.scale.name, "default");
}

#[tokio::test]
async fn undatable_rereads_are_reported_not_dropped_in_silence() {
    let (tmp, engine) = common::engine().await;
    let path = tmp_csv(
        tmp.path(),
        "undated.csv",
        "Title,Exclusive Shelf,Read Count\nSalt,read,3\n",
    );
    let report = engine.import_goodreads(&path, plain()).await.unwrap();

    let salt = book(&engine, "Salt").await;
    let readings = engine
        .storage()
        .list_readings(salt.id.unwrap())
        .await
        .unwrap();
    assert_eq!(readings.len(), 1);
    assert_eq!(readings[0].finished_at, None);
    assert_eq!(readings[0].status, "finished", "closed in status, undated");

    let dropped = report
        .warnings
        .iter()
        .find_map(|d| match &d.kind {
            DiagnosticKind::GoodreadsUndatableRereads { dropped, .. } => Some(*dropped),
            _ => None,
        })
        .expect("said so");
    assert_eq!(dropped, 2);
}

#[tokio::test]
async fn an_unmapped_rating_skips_its_row_and_says_why() {
    let (_tmp, engine) = common::engine().await;
    let id = common::seed_book(&engine, "Middlemarch").await;
    let review = engine.open_review(id, None).await.unwrap();
    let tens = engine
        .put_rating_scale("tens", 0.0, 10.0, 1.0)
        .await
        .unwrap();
    engine.set_rating(review.id, 7.0).await.unwrap();

    let (csv, warnings) = engine.export_goodreads().await.unwrap();
    assert!(!csv.contains("Middlemarch"), "the row was skipped:\n{csv}");
    assert!(
        warnings
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::GoodreadsRatingUnmapped { .. })),
        "{warnings:?}"
    );
    engine.map_rating(&tens, 7.0, 3.75).await.unwrap();
    let (csv, warnings) = engine.export_goodreads().await.unwrap();
    assert!(csv.contains("Middlemarch"));
    assert!(csv.contains(",3.75,"), "{csv}");
    assert!(warnings.is_empty());
}

#[tokio::test]
async fn a_quarter_star_round_trips_through_goodreads() {
    let (tmp, engine) = common::engine().await;
    let id = common::seed_book(&engine, "Middlemarch").await;
    let review = engine.open_review(id, None).await.unwrap();
    engine.set_rating(review.id, 4.25).await.unwrap();
    assert_eq!(
        engine.goodreads_rating(review.id).await.unwrap(),
        Some(4.25)
    );

    let (csv, warnings) = engine.export_goodreads().await.unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(csv.contains(",4.25,"), "{csv}");

    let path = tmp.path().join("export.csv");
    std::fs::write(&path, &csv).unwrap();
    let rows = parse_csv(&path).unwrap();
    assert_eq!(rows[0].my_rating, Some(4.25));
}

#[tokio::test]
async fn a_reread_exports_its_latest_reading_and_reports_the_rest() {
    let (_tmp, engine) = common::engine().await;
    engine
        .import_goodreads(&recorded("library-export.csv"), plain())
        .await
        .unwrap();

    let (csv, warnings) = engine.export_goodreads().await.unwrap();
    assert_eq!(csv.matches("Crime and Punishment").count(), 1);
    let dropped = warnings
        .iter()
        .find_map(|d| match &d.kind {
            DiagnosticKind::GoodreadsRereadsDropped { title, dropped }
                if title == "Crime and Punishment" =>
            {
                Some(*dropped)
            }
            _ => None,
        })
        .expect("said so");
    assert_eq!(dropped, 2);
}

/// Export, import, export is stable. It imports into a fresh library, since re-importing into
/// the source library would only prove the matcher found what it just wrote.
#[tokio::test]
async fn export_import_export_is_stable() {
    let (tmp_a, a) = common::engine().await;
    a.import_goodreads(&recorded("library-export.csv"), creating())
        .await
        .unwrap();
    let (first, _) = a.export_goodreads().await.unwrap();

    let (_tmp_b, b) = common::engine().await;
    let path = tmp_csv(tmp_a.path(), "round-trip.csv", &first);
    b.import_goodreads(&path, creating()).await.unwrap();
    let (second, _) = b.export_goodreads().await.unwrap();

    assert_eq!(first, second, "the round trip moved something");
    assert!(
        first.contains(r#""=""9781900000178""""#),
        "armoured:\n{first}"
    );
    assert!(
        first.contains("2020/04/18"),
        "the reading came back:\n{first}"
    );
}

#[tokio::test]
async fn the_generated_library_round_trips_and_re_imports_idempotently() {
    let (tmp, a) = common::engine().await;
    let report = a
        .import_goodreads(&generated(), creating())
        .await
        .expect("imports");
    assert_eq!(report.rows, 40);
    assert!(report.unmatched.is_empty());

    let before = snapshot(&a).await;
    let again = a.import_goodreads(&generated(), creating()).await.unwrap();
    assert_eq!(again.created(), 0);
    assert_eq!(snapshot(&a).await, before, "a second pass moved something");

    let (first, _) = a.export_goodreads().await.unwrap();
    let (_tmp_b, b) = common::engine().await;
    let path = tmp_csv(tmp.path(), "corpus-round-trip.csv", &first);
    b.import_goodreads(&path, creating()).await.unwrap();
    let (second, _) = b.export_goodreads().await.unwrap();
    assert_eq!(first, second);
}

async fn book(engine: &Engine, selector: &str) -> Book {
    let mut hits = engine.resolve_books(selector).await.expect("resolve");
    assert_eq!(hits.len(), 1, "{selector} resolved to {} books", hits.len());
    hits.remove(0)
}

async fn snapshot(engine: &Engine) -> Vec<String> {
    let mut out = Vec::new();
    for b in engine
        .storage()
        .list_books(&bookmole::BookQuery::new(1000, bookmole::BookSort::Title))
        .await
        .unwrap()
    {
        let id = b.id.unwrap();
        let readings = engine.storage().list_readings(id).await.unwrap();
        let tags = engine.storage().book_tags(id).await.unwrap();
        let mut notes: Vec<String> = engine
            .list_notes(NoteScope::Book(id), None)
            .await
            .unwrap()
            .into_iter()
            .map(|n| format!("{}:{}", n.kind, n.title))
            .collect();
        notes.sort();
        out.push(format!(
            "{}|{:?}|{:?}|{:?}|{notes:?}",
            b.display_title(),
            b.any_isbn(),
            readings
                .iter()
                .map(|r| (r.status.clone(), r.finished_at))
                .collect::<Vec<_>>(),
            tags.iter().map(|t| t.tag.clone()).collect::<Vec<_>>(),
        ));
    }
    out.sort();
    out
}

#[tokio::test]
async fn linking_a_goodreads_row_by_hand_is_matched_by_its_id_from_then_on() {
    let (_tmp, engine) = common::engine().await;
    let mine = engine
        .save_book(&Book {
            title: Some("A Novel About Snow".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    engine
        .link_goodreads_row("9000001", mine.id.unwrap())
        .await
        .unwrap();

    let report = engine
        .import_goodreads(&recorded("library-export.csv"), plain())
        .await
        .unwrap();

    let middlemarch = report
        .books
        .iter()
        .find(|b| b.title == "Middlemarch")
        .expect("the linked row imported");
    assert_eq!(middlemarch.matched_by, GoodreadsMatch::ExternalId);
    assert_eq!(middlemarch.book_id, mine.id);
    assert!(
        !report.unmatched.iter().any(|u| u.title == "Middlemarch"),
        "a linked row is never offered as a guess again"
    );
}

#[tokio::test]
async fn a_goodreads_row_with_no_id_cannot_be_linked() {
    let (_tmp, engine) = common::engine().await;
    let mine = engine
        .save_book(&common::book("Middlemarch"))
        .await
        .unwrap();
    let err = engine
        .link_goodreads_row("", mine.id.unwrap())
        .await
        .expect_err("an empty Book Id is not an identity");
    assert!(
        matches!(err, bookmole::EngineError::InvalidInput(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_goodreads_id_and_a_calibre_uuid_do_not_collide() {
    let (_tmp, engine) = common::engine().await;
    let a = engine.save_book(&common::book("First")).await.unwrap();
    let b = engine.save_book(&common::book("Second")).await.unwrap();

    engine
        .link_goodreads_row("shared", a.id.unwrap())
        .await
        .unwrap();
    engine
        .link_calibre_book("shared", b.id.unwrap())
        .await
        .unwrap();

    assert_eq!(
        engine
            .storage()
            .book_for_external_id("goodreads", "shared")
            .await
            .unwrap(),
        a.id
    );
    assert_eq!(
        engine
            .storage()
            .book_for_external_id("calibre", "shared")
            .await
            .unwrap(),
        b.id
    );
}

fn unix(day: &str) -> i64 {
    bookmole::history::midnight(bookmole::history::parse_day(day).unwrap())
}

async fn on_timeline(engine: &Engine, book_id: i64) -> bool {
    engine
        .history_timeline(&bookmole::HistorySpan::default())
        .await
        .unwrap()
        .iter()
        .any(|r| r.reading.book_id == book_id)
}

#[tokio::test]
async fn a_date_started_column_places_the_read_and_its_absence_is_silent() {
    let (tmp, engine) = common::engine().await;
    let path = tmp_csv(
        tmp.path(),
        "started.csv",
        "Title,Author,Exclusive Shelf,Date Started,Date Read\n\
         Middlemarch,George Eliot,read,2024/01/05,2024/03/27\n\
         Silas Marner,George Eliot,read,,2024/05/01\n\
         Romola,George Eliot,read,2024/09/01,2024/08/01\n\
         Adam Bede,George Eliot,currently-reading,2024/10/20,\n",
    );
    let report = engine.import_goodreads(&path, creating()).await.unwrap();
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let both = book(&engine, "Middlemarch").await.id.unwrap();
    let r = engine.list_readings(both).await.unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].started_at, Some(unix("2024-01-05")));
    assert_eq!(r[0].finished_at, Some(unix("2024-03-27")));
    assert!(on_timeline(&engine, both).await);

    let finish_only = book(&engine, "Silas Marner").await.id.unwrap();
    let r = engine.list_readings(finish_only).await.unwrap();
    assert_eq!(r.len(), 1, "imported and listed in the book's reads");
    assert_eq!(r[0].started_at, None, "no start is invented");
    assert_eq!(r[0].finished_at, Some(unix("2024-05-01")));
    assert!(!on_timeline(&engine, finish_only).await);
    let figures = engine
        .history_figures(&bookmole::HistorySpan::default())
        .await
        .unwrap();
    assert_eq!(figures.books_finished, 3, "every finished read is counted");

    let backwards = book(&engine, "Romola").await.id.unwrap();
    let r = engine.list_readings(backwards).await.unwrap();
    assert_eq!(r[0].started_at, None, "a start after the finish is dropped");

    let open = book(&engine, "Adam Bede").await.id.unwrap();
    let r = engine.list_readings(open).await.unwrap();
    assert_eq!(r[0].started_at, Some(unix("2024-10-20")));
    assert!(on_timeline(&engine, open).await);
}

#[tokio::test]
async fn a_later_export_with_a_start_fills_the_read_already_imported() {
    let (tmp, engine) = common::engine().await;
    let first = tmp_csv(
        tmp.path(),
        "first.csv",
        "Title,Author,Exclusive Shelf,Date Read\nMiddlemarch,George Eliot,read,2024/03/27\n",
    );
    engine.import_goodreads(&first, creating()).await.unwrap();
    let later = tmp_csv(
        tmp.path(),
        "later.csv",
        "Title,Author,Exclusive Shelf,Date Started,Date Read\n\
         Middlemarch,George Eliot,read,2024/01/05,2024/03/27\n",
    );
    let report = engine.import_goodreads(&later, creating()).await.unwrap();
    assert_eq!(report.books[0].readings_added, 0);

    let id = book(&engine, "Middlemarch").await.id.unwrap();
    let r = engine.list_readings(id).await.unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].started_at, Some(unix("2024-01-05")));
}

#[tokio::test]
async fn an_undated_finished_row_is_not_being_read() {
    let (tmp, engine) = common::engine().await;
    let path = tmp_csv(
        tmp.path(),
        "undated-read.csv",
        "Title,Author,Exclusive Shelf\nMiddlemarch,George Eliot,read\n",
    );
    engine.import_goodreads(&path, creating()).await.unwrap();
    let id = book(&engine, "Middlemarch").await.id.unwrap();

    let r = engine.list_readings(id).await.unwrap();
    assert_eq!(r.len(), 1, "the read is kept, as imported");
    assert_eq!((r[0].status.as_str(), r[0].finished_at), ("finished", None));

    assert!(engine.currently_reading(20).await.unwrap().is_empty());
    assert!(engine.history_reading_now().await.unwrap().is_empty());
    assert!(!on_timeline(&engine, id).await);
}
