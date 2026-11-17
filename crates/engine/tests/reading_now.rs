//! The open read of one book, through the facade: finishing it on a named
//! day, and saying how many pages the edition has. Both are new doors; reading again is `reread`, which is not.
//!
//! Offline, `sqlite::memory:` plus a `TempDir` vault.

mod common;
use bookmole::{EngineError, Progress};
use common::{engine, seed_book};

/// Today, UTC, as the engine's days are spelled.
fn today() -> String {
    time::OffsetDateTime::now_utc().date().to_string()
}

/// Finishing is about the read you are in. With none open there is nothing to
/// close, and — the reason this is not `update_progress(finished)` — nothing
/// is opened in order to be closed.
#[tokio::test]
async fn finishing_a_book_with_no_open_read_is_refused_and_writes_nothing() {
    let (_tmp, engine) = engine().await;
    let id = seed_book(&engine, "Kokoro").await;

    let refused = engine.finish_reading_on(id, None).await;
    assert!(
        matches!(refused, Err(EngineError::InvalidInput(_))),
        "{refused:?}"
    );
    assert!(
        engine.list_readings(id).await.unwrap().is_empty(),
        "a refused finish must not mint a read to finish"
    );
}

#[tokio::test]
async fn finishing_on_today_closes_the_open_read_and_reading_again_opens_a_new_one() {
    let (_tmp, engine) = engine().await;
    let id = seed_book(&engine, "Station Eleven").await;
    engine.update_progress(id, Some(120), None).await.unwrap();
    let first = engine.active_reading(id).await.unwrap().unwrap();

    let book = engine.finish_reading_on(id, Some(&today())).await.unwrap();
    assert!(book.finished, "the book projects its finished read");
    assert!(matches!(Progress::of_book(&book), Progress::Finished));
    assert!(engine.active_reading(id).await.unwrap().is_none());
    let closed = engine.get_reading(first.id).await.unwrap().unwrap();
    assert!(closed.finished_at.is_some());
    assert!(
        closed.finished_at >= first.started_at,
        "a read finished today, the day it began, finishes no earlier than it began"
    );

    // Read it again: a second read, and the first is still there, finished.
    let second = engine.reread(id).await.unwrap();
    assert_ne!(second, first.id);
    let readings = engine.list_readings(id).await.unwrap();
    assert_eq!(readings.len(), 2);
    assert!(
        readings
            .iter()
            .any(|r| r.id == first.id && r.finished_at.is_some())
    );
    let open = engine.active_reading(id).await.unwrap().unwrap();
    assert_eq!(open.id, second);
    assert_eq!(open.current_page, None, "a new read starts at its start");
}

#[tokio::test]
async fn a_day_before_the_read_began_is_refused_and_the_read_stays_open() {
    let (_tmp, engine) = engine().await;
    let id = seed_book(&engine, "Middlemarch").await;
    engine.update_progress(id, Some(3), None).await.unwrap();

    let refused = engine.finish_reading_on(id, Some("2001-01-01")).await;
    assert!(
        matches!(refused, Err(EngineError::InvalidInput(_))),
        "{refused:?}"
    );
    assert!(engine.active_reading(id).await.unwrap().is_some());

    let garbled = engine.finish_reading_on(id, Some("yesterday")).await;
    assert!(
        matches!(garbled, Err(EngineError::InvalidInput(_))),
        "{garbled:?}"
    );
}

/// The second box of *Page [N] of [M]*: the user's number, held as theirs, and
/// the progress the engine words from it gains a denominator.
#[tokio::test]
async fn a_page_count_the_reader_sets_is_theirs_and_gives_progress_a_length() {
    let (_tmp, engine) = engine().await;
    let id = seed_book(&engine, "The Dispossessed").await;
    engine.update_progress(id, Some(80), None).await.unwrap();

    let book = engine.set_page_count(id, 320).await.unwrap();
    assert_eq!(book.page_count, Some(320));
    match Progress::of_book(&book) {
        Progress::Started { page, of, fraction } => {
            assert_eq!((page, of), (Some(80), Some(320)));
            assert!(fraction.is_some());
        }
        other => panic!("expected a started book, got {other:?}"),
    }
    let claims = engine.field_provenance(id).await.unwrap();
    assert!(
        claims
            .iter()
            .any(|c| c.field == "page_count" && c.source == "user"),
        "{claims:?}"
    );
    assert!(
        !claims
            .iter()
            .any(|c| c.field == "title" && c.source == "user"),
        "setting one number claims one field"
    );

    for bad in [0, -4] {
        let refused = engine.set_page_count(id, bad).await;
        assert!(
            matches!(refused, Err(EngineError::InvalidInput(_))),
            "{bad}: {refused:?}"
        );
    }
    assert_eq!(
        engine.get_book(id).await.unwrap().unwrap().page_count,
        Some(320)
    );

    let missing = engine.set_page_count(9_999, 10).await;
    assert!(
        matches!(missing, Err(EngineError::NotFound(_))),
        "{missing:?}"
    );
}
