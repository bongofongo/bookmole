//! KOReader `statistics.sqlite3` import. The fixture's `expected.json` totals are accumulated by
//! the generator while it writes rows, so they are an independent oracle.

mod common;

use std::path::{Path, PathBuf};

use bookmole::storage::LinkedBy;
use bookmole::{Confidence, DayRange, Engine};
use common::engine;

use serde::Deserialize;
use sqlx::Connection;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/koreader/statistics"
);

#[derive(Deserialize)]
struct Expected {
    schema_version: i64,
    books: Vec<ExpectedBook>,
}

#[derive(Deserialize)]
struct ExpectedBook {
    title: String,
    md5: Option<String>,
    in_library: bool,
    days: Vec<ExpectedDay>,
}

#[derive(Deserialize, Clone)]
struct ExpectedDay {
    day: String,
    #[allow(dead_code)]
    seconds: i64,
    pages: i64,
    minutes: i64,
}

fn expected() -> Expected {
    let raw = std::fs::read_to_string(Path::new(FIXTURE).join("expected.json"))
        .expect("the fixture is committed; run `make kostats` if it is not");
    serde_json::from_str(&raw).expect("expected.json parses")
}

/// Builds a real SQLite database: the module reads `user_version` and uses `date(.., 'unixepoch')`.
async fn build_db(at: &Path) {
    let script = std::fs::read_to_string(Path::new(FIXTURE).join("statistics.sql"))
        .expect("the fixture script is committed");
    let mut conn = open(at, true).await;
    sqlx::raw_sql(&script)
        .execute(&mut conn)
        .await
        .expect("the plugin's own DDL applies");
    conn.close().await.expect("close cleanly");
}

async fn open(at: &Path, create: bool) -> sqlx::SqliteConnection {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(at)
        .create_if_missing(create);
    sqlx::SqliteConnection::connect_with(&opts)
        .await
        .expect("open")
}

fn install(root: &Path) -> PathBuf {
    let dir = root.join("koreader");
    std::fs::create_dir_all(dir.join("frontend")).unwrap();
    std::fs::create_dir_all(dir.join("plugins")).unwrap();
    std::fs::create_dir_all(dir.join("settings")).unwrap();
    std::fs::write(dir.join("reader.lua"), "-- entry point\n").unwrap();
    dir.join("settings/statistics.sqlite3")
}

async fn seed_library(engine: &Engine, exp: &Expected) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for b in &exp.books {
        if !b.in_library {
            continue;
        }
        let id = common::seed_book(engine, &b.title).await;
        let md5 = b.md5.as_ref().expect("an in-library book has an md5");
        engine
            .storage()
            .link_device_book(md5, id, LinkedBy::Auto)
            .await
            .unwrap();
        out.push((b.title.clone(), id));
    }
    out
}

#[tokio::test]
async fn measured_days_land_as_events_with_the_generators_own_totals() {
    let (tmp, engine) = engine().await;
    let exp = expected();
    let db = install(tmp.path());
    build_db(&db).await;
    let books = seed_library(&engine, &exp).await;

    let report = bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .unwrap();

    assert_eq!(report.schema_version, Some(exp.schema_version));
    assert_eq!(report.books_in_db, exp.books.len());
    assert_eq!(report.books_matched, books.len());

    for (title, id) in &books {
        let want = exp
            .books
            .iter()
            .find(|b| &b.title == title)
            .expect("seeded from the same list");
        let got = engine.reading_events(*id).await.unwrap();

        assert_eq!(
            got.len(),
            want.days.len(),
            "{title}: one event per measured day, no more"
        );
        for (g, w) in got.iter().zip(want.days.iter()) {
            assert_eq!(g.day, w.day, "{title}");
            assert_eq!(g.minutes, Some(w.minutes), "{title} on {}", w.day);
            assert_eq!(g.pages, Some(w.pages), "{title} on {}", w.day);
            assert_eq!(g.source, "koreader");
            assert_eq!(
                g.confidence,
                Confidence::Measured,
                "the device measured this; nothing here is inferred"
            );
        }
    }
}

#[tokio::test]
async fn a_page_revisited_the_same_day_counts_once() {
    let (tmp, engine) = engine().await;
    let exp = expected();
    let db = install(tmp.path());
    build_db(&db).await;
    let books = seed_library(&engine, &exp).await;

    bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .unwrap();

    let (_, id) = books.iter().find(|(t, _)| t == "Middlemarch").unwrap();
    let events = engine.reading_events(*id).await.unwrap();
    let second = events.iter().find(|e| e.day == "2026-01-06").unwrap();
    assert_eq!(
        second.pages,
        Some(2),
        "three rows, two distinct pages — a revisit is not a page turn"
    );
}

#[tokio::test]
async fn a_zero_start_time_does_not_become_a_day_in_1970() {
    let (tmp, engine) = engine().await;
    let exp = expected();
    let db = install(tmp.path());
    build_db(&db).await;
    let books = seed_library(&engine, &exp).await;

    bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .unwrap();

    for (_, id) in &books {
        for e in engine.reading_events(*id).await.unwrap() {
            assert!(
                e.day.starts_with("2026-"),
                "a default start_time invented the day {}",
                e.day
            );
        }
    }
}

#[tokio::test]
async fn a_device_with_no_statistics_database_is_ordinary() {
    let (tmp, engine) = engine().await;
    let db = install(tmp.path());
    let report = bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .expect("absence is not an error");

    assert_eq!(report.schema_version, None);
    assert_eq!(report.days, 0);
    assert_eq!(report.events.inserted, 0);
    assert!(matches!(
        report.warnings.as_slice(),
        [d] if matches!(d.kind, bookmole::DiagnosticKind::StatisticsDbAbsent { .. })
    ));
}

#[tokio::test]
async fn an_unknown_schema_version_imports_nothing() {
    let (tmp, engine) = engine().await;
    let exp = expected();
    let db = install(tmp.path());
    build_db(&db).await;
    seed_library(&engine, &exp).await;

    let mut conn = open(&db, false).await;
    sqlx::raw_sql("PRAGMA user_version=20991231;")
        .execute(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();

    let report = bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .unwrap();
    assert_eq!(report.schema_version, Some(20991231));
    assert_eq!(report.events.inserted, 0, "nothing was written");
    assert_eq!(report.days, 0);
    assert!(report.warnings.iter().any(|d| matches!(
        d.kind,
        bookmole::DiagnosticKind::StatisticsSchemaUnknown {
            version: 20991231,
            ..
        }
    )));
}

#[tokio::test]
async fn a_file_that_is_not_a_database_degrades() {
    let (tmp, engine) = engine().await;
    let db = install(tmp.path());
    std::fs::write(&db, b"this is not a database, it is a note to self").unwrap();

    let report = bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .expect("a corrupt file is a diagnostic, not an error");
    assert_eq!(report.events.inserted, 0);
    assert!(report.warnings.iter().any(|d| matches!(
        d.kind,
        bookmole::DiagnosticKind::StatisticsDbUnreadable { .. }
    )));
}

#[tokio::test]
async fn books_we_do_not_hold_and_rows_with_no_md5_are_both_reported() {
    let (tmp, engine) = engine().await;
    let exp = expected();
    let db = install(tmp.path());
    build_db(&db).await;
    seed_library(&engine, &exp).await;

    let report = bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .unwrap();

    let unmatched = exp
        .books
        .iter()
        .find(|b| !b.in_library && b.md5.is_some())
        .unwrap();
    assert!(
        report.warnings.iter().any(|d| matches!(
            &d.kind,
            bookmole::DiagnosticKind::StatisticsBookUnmatched { md5 }
                if Some(md5.as_str()) == unmatched.md5.as_deref()
        )),
        "a book on the device that is not in the library must be named"
    );
    assert!(
        report.warnings.iter().any(|d| matches!(
            d.kind,
            bookmole::DiagnosticKind::StatisticsBookNotIdentified { .. }
        )),
        "a statistics row with no md5 has nothing to join on, and says so"
    );
}

#[tokio::test]
async fn re_importing_the_same_device_changes_nothing() {
    let (tmp, engine) = engine().await;
    let exp = expected();
    let db = install(tmp.path());
    build_db(&db).await;
    let books = seed_library(&engine, &exp).await;

    let first = bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .unwrap();
    assert!(first.events.inserted > 0);

    let before: Vec<_> = {
        let mut all = Vec::new();
        for (_, id) in &books {
            all.extend(engine.reading_events(*id).await.unwrap());
        }
        all
    };

    let second = bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .unwrap();
    assert_eq!(second.events.inserted, 0, "a re-import duplicated a day");
    assert_eq!(
        second.events.updated, 0,
        "a re-import rewrote an unchanged row, so idempotency is invisible"
    );

    let after: Vec<_> = {
        let mut all = Vec::new();
        for (_, id) in &books {
            all.extend(engine.reading_events(*id).await.unwrap());
        }
        all
    };
    assert_eq!(after, before);
}

/// Measured days must update the highlight filler's `source = 'koreader'` rows in place; a
/// delete-then-insert by source would wipe its inferred days.
#[tokio::test]
async fn measured_minutes_upgrade_the_highlight_fillers_inferred_days() {
    let (tmp, engine) = engine().await;
    let exp = expected();
    let db = install(tmp.path());
    build_db(&db).await;
    let books = seed_library(&engine, &exp).await;
    let (_, middlemarch) = books.iter().find(|(t, _)| t == "Middlemarch").unwrap();

    for when in ["2026-01-05 08:00:00", "2026-02-20 21:00:00"] {
        engine
            .storage()
            .insert_highlight(*middlemarch, &common::highlight(when, when))
            .await
            .unwrap();
    }
    engine.refill_reading_events().await.unwrap();

    let inferred = engine.reading_events(*middlemarch).await.unwrap();
    assert_eq!(inferred.len(), 2);
    assert!(
        inferred
            .iter()
            .all(|e| e.confidence == Confidence::Inferred)
    );
    assert!(inferred.iter().all(|e| e.minutes.is_none()));

    bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .unwrap();

    let after = engine.reading_events(*middlemarch).await.unwrap();
    let jan5 = after.iter().find(|e| e.day == "2026-01-05").unwrap();
    assert_eq!(
        jan5.confidence,
        Confidence::Measured,
        "confidence ratchets up on the row the highlight filler already made"
    );
    assert_eq!(jan5.minutes, Some(12));

    let feb = after.iter().find(|e| e.day == "2026-02-20").unwrap();
    assert_eq!(
        feb.confidence,
        Confidence::Inferred,
        "a day the device measured nothing keeps the day it inferred"
    );
    assert_eq!(
        feb.minutes, None,
        "and it still has no minutes — absent, not zero"
    );

    assert_eq!(
        after.iter().filter(|e| e.day == "2026-01-05").count(),
        1,
        "the statistics filler added a parallel row instead of merging"
    );
}

/// The connection stays open across the import: a clean close checkpoints the `-wal` away,
/// and the rows must exist only there.
#[tokio::test]
async fn rows_still_in_the_write_ahead_log_are_imported() {
    let (tmp, engine) = engine().await;
    let exp = expected();
    let db = install(tmp.path());
    build_db(&db).await;
    let books = seed_library(&engine, &exp).await;
    let (_, middlemarch) = books.iter().find(|(t, _)| t == "Middlemarch").unwrap();

    let mut live = open(&db, false).await;
    sqlx::raw_sql(
        "PRAGMA journal_mode=WAL;
         INSERT INTO page_stat_data (id_book, page, start_time, duration, total_pages)
         VALUES (1, 200, 1769040000, 1800, 300);",
    )
    .execute(&mut live)
    .await
    .unwrap();

    assert!(
        db.with_extension("sqlite3-wal").exists() || wal_beside(&db).exists(),
        "the test needs a real -wal file to be meaningful"
    );

    let report = bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .unwrap();
    assert_eq!(report.schema_version, Some(exp.schema_version));

    let events = engine.reading_events(*middlemarch).await.unwrap();
    assert!(
        events.iter().any(|e| e.day == "2026-01-22"),
        "the session still in the write-ahead log was lost: {:?}",
        events.iter().map(|e| &e.day).collect::<Vec<_>>()
    );

    live.close().await.unwrap();
}

fn wal_beside(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push("-wal");
    PathBuf::from(name)
}

#[tokio::test]
async fn a_day_no_single_read_holds_is_left_unattributed() {
    let (tmp, engine) = engine().await;
    let exp = expected();
    let db = install(tmp.path());
    build_db(&db).await;
    let books = seed_library(&engine, &exp).await;
    let (_, middlemarch) = books.iter().find(|(t, _)| t == "Middlemarch").unwrap();

    let jan5 = 1_767_571_200i64;
    engine
        .storage()
        .record_reading(
            *middlemarch,
            Some(jan5 - 10 * 86_400),
            Some(jan5 + 3_600),
            "finished",
            "manual",
        )
        .await
        .unwrap();
    engine
        .storage()
        .record_reading(
            *middlemarch,
            Some(jan5 + 7_200),
            Some(jan5 + 20 * 86_400),
            "finished",
            "manual",
        )
        .await
        .unwrap();

    bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .unwrap();

    let events = engine.reading_events(*middlemarch).await.unwrap();
    let straddling = events.iter().find(|e| e.day == "2026-01-05").unwrap();
    assert_eq!(
        straddling.reading_id, None,
        "a day two reads both hold cannot be split at this grain, and guessing \
         gives one read the other's minutes with nothing looking wrong"
    );

    let later = events.iter().find(|e| e.day == "2026-01-09").unwrap();
    assert!(
        later.reading_id.is_some(),
        "an unambiguous day is attributable"
    );
}

#[tokio::test]
async fn a_month_the_device_never_measured_has_absent_minutes_not_zero() {
    let (tmp, engine) = engine().await;
    let exp = expected();
    let db = install(tmp.path());
    build_db(&db).await;
    seed_library(&engine, &exp).await;

    bookmole::ko_statistics::import_statistics(engine.storage(), &db)
        .await
        .unwrap();

    let jan = DayRange::new("2026-01-01", "2026-01-31").unwrap();
    let sum = engine.activity_summary(&jan).await.unwrap();
    assert!(sum.minutes.unwrap() > 0, "January was measured");

    let march = DayRange::new("2026-03-01", "2026-03-31").unwrap();
    let sum = engine.activity_summary(&march).await.unwrap();
    assert_eq!(
        sum.minutes, None,
        "a month with no device data has no minutes; zero would be a claim"
    );
    assert_eq!(sum.pages, None);
    assert_eq!(sum.activity_days, 0);
}
