//! The home-screen surface through the facade: it answers without reaching into
//! `engine.storage`, and opening a reflection returns the record every editor path takes.

mod common;
use bookmole::NoteScope;
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
