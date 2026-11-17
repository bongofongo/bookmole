//! The home-screen surface through the facade: it answers without reaching into
//! `engine.storage`, and opening a reflection returns the record every editor path takes.

mod common;
use bookmole::{EngineError, NoteScope};
use common::{engine, seed_book};

#[tokio::test]
async fn the_facade_lists_what_is_open_and_nothing_else() {
    let (_tmp, engine) = engine().await;
    let middlemarch = seed_book(&engine, "Middlemarch").await;
    let kokoro = seed_book(&engine, "Kokoro").await;

    assert!(
        engine.currently_reading(20).await.unwrap().is_empty(),
        "an untouched library is an empty home screen, not a full one"
    );

    engine
        .storage()
        .update_progress(middlemarch, Some(120), None)
        .await
        .unwrap();
    let open = engine.currently_reading(20).await.unwrap();
    assert_eq!(open.len(), 1);
    let (book, reading) = &open[0];
    assert_eq!(book.id, Some(middlemarch));
    assert_eq!(reading.book_id, middlemarch);
    assert_eq!(reading.finished_at, None);
    assert!(!open.iter().any(|(b, _)| b.id == Some(kokoro)));

    engine
        .storage()
        .update_progress(middlemarch, None, Some(true))
        .await
        .unwrap();
    assert!(engine.currently_reading(20).await.unwrap().is_empty());

    let second = engine.storage().reread(middlemarch).await.unwrap();
    let open = engine.currently_reading(20).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].1.id, second);
}

#[tokio::test]
async fn the_book_and_its_reading_are_the_same_reading() {
    let (_tmp, engine) = engine().await;
    let book_id = seed_book(&engine, "Middlemarch").await;
    engine
        .storage()
        .update_progress(book_id, Some(490), Some(true))
        .await
        .unwrap();
    engine.storage().reread(book_id).await.unwrap();
    engine
        .storage()
        .update_progress(book_id, Some(40), None)
        .await
        .unwrap();

    let (book, reading) = engine.currently_reading(20).await.unwrap().remove(0);
    assert_eq!(book.current_page, reading.current_page);
    assert_eq!(book.current_page, Some(40));
    assert_eq!(book.date_started, reading.started_at);
    assert_eq!(book.date_finished, reading.finished_at);
    assert!(!book.finished);
}

#[tokio::test]
async fn the_record_wrapper_opens_the_same_reflection() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;

    let created = engine.open_reflection(book, None).await.unwrap();
    let record = engine.open_reflection_record(book, None).await.unwrap();
    assert_eq!(record.id, created.id);
    assert_eq!(record.title, created.title);
    assert_eq!(record.kind, "reflection");
    assert_eq!(
        engine
            .list_notes(NoteScope::Book(book), None)
            .await
            .unwrap()
            .len(),
        1
    );

    assert_eq!(engine.note_body(&record).unwrap(), "");
    engine
        .update_note_body(&record, "Dorothea's dignity, chapter by chapter.")
        .await
        .unwrap();

    let again = engine.open_reflection_record(book, None).await.unwrap();
    assert_eq!(again.id, record.id);
    assert_eq!(
        engine.note_body(&again).unwrap(),
        "Dorothea's dignity, chapter by chapter."
    );
}

#[tokio::test]
async fn the_review_twin_is_its_own_note() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;

    let reflection = engine.open_reflection_record(book, None).await.unwrap();
    let review = engine.open_review_record(book, None).await.unwrap();
    assert_ne!(review.id, reflection.id);
    assert_eq!(review.kind, "review");
    assert_eq!(review.reading_id, reflection.reading_id);
    assert_eq!(
        engine.open_review_record(book, None).await.unwrap().id,
        review.id,
        "one review per reading, and reopening finds it"
    );
    assert_eq!(
        engine
            .list_notes(NoteScope::Book(book), None)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(engine.storage().list_readings(book).await.unwrap().len(), 1);
}

#[tokio::test]
async fn opening_from_a_finished_book_does_not_reopen_it() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    engine
        .storage()
        .update_progress(book, Some(490), Some(true))
        .await
        .unwrap();

    let record = engine.open_reflection_record(book, None).await.unwrap();
    let readings = engine.storage().list_readings(book).await.unwrap();
    assert_eq!(readings.len(), 1);
    assert_eq!(record.reading_id, Some(readings[0].id));
    assert!(
        engine.currently_reading(20).await.unwrap().is_empty(),
        "reflecting on a finished book must not put it back on the shelf"
    );
}

async fn places(engine: &bookmole::Engine, book: i64) -> [bool; 3] {
    let current = engine
        .currently_reading(20)
        .await
        .unwrap()
        .iter()
        .any(|(b, _)| b.id == Some(book));
    let now = engine
        .history_reading_now()
        .await
        .unwrap()
        .iter()
        .any(|r| r.reading.book_id == book);
    let timeline = engine
        .history_timeline(&bookmole::HistorySpan::default())
        .await
        .unwrap()
        .iter()
        .any(|r| r.reading.book_id == book);
    [current, now, timeline]
}

#[tokio::test]
async fn a_put_down_read_leaves_every_place_and_picking_it_up_restores_it() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    engine.update_progress(book, Some(120), None).await.unwrap();
    assert_eq!(places(&engine, book).await, [true; 3]);

    let (down, _) = engine.put_down(book).await.unwrap();
    assert_eq!(down.status, "abandoned");
    assert!(down.put_down_at.is_some());
    assert_eq!(down.finished_at, None, "put down is not finished");
    assert_eq!(places(&engine, book).await, [false; 3]);

    let listed = engine.list_readings(book).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, "abandoned");
    assert_eq!(listed[0].put_down_at, down.put_down_at);
    assert_eq!(listed[0].current_page, Some(120));

    let (up, _) = engine.pick_up(book).await.unwrap();
    assert_eq!(up.id, down.id, "picking up is the same read, not a reread");
    assert_eq!(up.status, "reading");
    assert_eq!(up.put_down_at, None);
    assert_eq!(places(&engine, book).await, [true; 3]);
}

#[tokio::test]
async fn putting_down_and_picking_up_refuse_what_does_not_fit() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    let refused = |r: bookmole::Result<_>| matches!(r, Err(EngineError::InvalidInput(_)));

    assert!(refused(engine.put_down(book).await), "no open read");
    assert!(refused(engine.pick_up(book).await), "no open read");
    assert!(matches!(
        engine.put_down(book + 1000).await,
        Err(EngineError::NotFound(_))
    ));

    engine.update_progress(book, Some(10), None).await.unwrap();
    assert!(refused(engine.pick_up(book).await), "not put down");
    let (first, _) = engine.put_down(book).await.unwrap();
    assert!(refused(engine.put_down(book).await), "already put down");
    let again = engine.list_readings(book).await.unwrap().remove(0);
    assert_eq!(
        again.put_down_at, first.put_down_at,
        "a refusal moves nothing"
    );

    engine
        .update_progress(book, None, Some(true))
        .await
        .unwrap();
    assert!(
        refused(engine.pick_up(book).await),
        "a finished read is not put down"
    );
}

#[tokio::test]
async fn what_the_other_reading_verbs_do_to_a_put_down_read() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    engine.update_progress(book, Some(10), None).await.unwrap();

    let (down, _) = engine.put_down(book).await.unwrap();
    engine.update_progress(book, Some(30), None).await.unwrap();
    let still = engine.list_readings(book).await.unwrap().remove(0);
    assert_eq!(
        (still.status.as_str(), still.current_page, still.put_down_at),
        ("abandoned", Some(30), down.put_down_at),
        "a page alone is recorded and leaves the read put down"
    );

    engine
        .update_progress(book, None, Some(false))
        .await
        .unwrap();
    let up = engine.list_readings(book).await.unwrap().remove(0);
    assert_eq!((up.status.as_str(), up.put_down_at), ("reading", None));

    engine.put_down(book).await.unwrap();
    let new = engine.reread(book).await.unwrap();
    let reads = engine.list_readings(book).await.unwrap();
    assert_eq!(reads.len(), 2);
    let old = reads.iter().find(|r| r.id != new).unwrap();
    assert_eq!(
        old.status, "abandoned",
        "a reread does not call the put-down read finished"
    );
    assert!(old.finished_at.is_some() && old.put_down_at.is_some());
    assert_eq!(
        old.finished_at, old.put_down_at,
        "closed where it was put down"
    );
    assert!(places(&engine, book).await[0], "the new read is current");

    engine.put_down(book).await.unwrap();
    engine.finish_reading_on(book, None).await.unwrap();
    let finished = engine
        .list_readings(book)
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.id == new)
        .unwrap();
    assert_eq!(
        finished.status, "finished",
        "finishing a put-down read finishes it"
    );
    assert_eq!(finished.put_down_at, None);
}
