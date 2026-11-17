//! Reflection and Review through the facade.

use bookmole::{Book, Engine, EngineError, NewNoteInput, NoteKind, NoteScope};

mod common;
use common::{engine, highlight, seed_book};

async fn seeded(engine: &Engine) -> i64 {
    seed_book(engine, "Middlemarch").await
}

#[tokio::test]
async fn opening_a_reflection_twice_returns_the_same_note_and_file() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;

    let first = engine.open_reflection(book, None).await.unwrap();
    assert!(first.file.exists());
    let record = engine.storage().get_note(first.id).await.unwrap().unwrap();
    engine
        .update_note_body(&record, "Dorothea's dignity, chapter by chapter.")
        .await
        .unwrap();

    let again = engine.open_reflection(book, None).await.unwrap();
    assert_eq!(again.id, first.id);
    assert_eq!(again.file, first.file);
    assert_eq!(
        engine.note_body(&record).unwrap(),
        "Dorothea's dignity, chapter by chapter.",
        "reopening must not blank what was written"
    );
    assert_eq!(
        engine
            .list_notes(NoteScope::Book(book), None)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_reflection_opens_a_reading_when_the_book_has_none() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;
    assert!(
        engine
            .storage()
            .list_readings(book)
            .await
            .unwrap()
            .is_empty()
    );

    let note = engine.open_reflection(book, None).await.unwrap();
    let readings = engine.storage().list_readings(book).await.unwrap();
    assert_eq!(readings.len(), 1);
    let record = engine.storage().get_note(note.id).await.unwrap().unwrap();
    assert_eq!(record.reading_id, Some(readings[0].id));
    assert_eq!(record.kind, "reflection");
}

#[tokio::test]
async fn a_finished_book_reflects_on_the_reading_it_just_finished() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;
    engine
        .storage()
        .update_progress(book, Some(490), Some(true))
        .await
        .unwrap();
    let readings = engine.storage().list_readings(book).await.unwrap();

    let note = engine.open_reflection(book, None).await.unwrap();
    let record = engine.storage().get_note(note.id).await.unwrap().unwrap();
    assert_eq!(record.reading_id, Some(readings[0].id));
    assert_eq!(
        engine.storage().list_readings(book).await.unwrap().len(),
        1,
        "reflecting on a finished book must not start a reread"
    );
}

#[tokio::test]
async fn a_reread_gets_its_own_pair_and_the_first_survives() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;

    let first_reflection = engine.open_reflection(book, None).await.unwrap();
    let first_review = engine.open_review(book, None).await.unwrap();
    let record = engine
        .storage()
        .get_note(first_reflection.id)
        .await
        .unwrap()
        .unwrap();
    engine
        .update_note_body(&record, "First time through.")
        .await
        .unwrap();
    engine
        .storage()
        .update_progress(book, Some(490), Some(true))
        .await
        .unwrap();

    engine.storage().reread(book).await.unwrap();
    let second_reflection = engine.open_reflection(book, None).await.unwrap();
    let second_review = engine.open_review(book, None).await.unwrap();

    assert_ne!(second_reflection.id, first_reflection.id);
    assert_ne!(second_review.id, first_review.id);
    assert_ne!(second_reflection.file, first_reflection.file);
    // Titles are wikilink targets; a reread's must not collide with the first reading's.
    assert_ne!(second_reflection.title, first_reflection.title);

    let readings = engine.storage().list_readings(book).await.unwrap();
    assert_eq!(readings.len(), 2);
    let notes = engine
        .list_notes(NoteScope::Book(book), None)
        .await
        .unwrap();
    assert_eq!(notes.len(), 4);
    assert_eq!(
        engine.note_body(&record).unwrap(),
        "First time through.",
        "the first reading's reflection is untouched by the second"
    );
    assert!(readings[0].finished_at.is_some());
}

#[tokio::test]
async fn an_explicit_reading_must_belong_to_the_book() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;
    let other = engine
        .save_book(&Book {
            title: Some("1Q84".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    let stray = engine
        .storage()
        .open_reading(other, Some(1), "manual")
        .await
        .unwrap();

    assert!(matches!(
        engine.open_reflection(book, Some(stray)).await,
        Err(EngineError::InvalidInput(_))
    ));
    assert!(matches!(
        engine.open_reflection(book, Some(9_999)).await,
        Err(EngineError::NotFound(_))
    ));
}

#[tokio::test]
async fn a_reflection_and_a_review_never_share_a_body() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;

    let reflection = engine.open_reflection(book, None).await.unwrap();
    let review = engine.open_review(book, None).await.unwrap();
    assert_ne!(reflection.id, review.id);
    assert_ne!(reflection.file, review.file);

    let r_rec = engine
        .storage()
        .get_note(reflection.id)
        .await
        .unwrap()
        .unwrap();
    let v_rec = engine.storage().get_note(review.id).await.unwrap().unwrap();
    engine
        .update_note_body(&r_rec, "Private: it made me think about my mother.")
        .await
        .unwrap();
    engine
        .update_note_body(&v_rec, "Public: a patient, generational novel.")
        .await
        .unwrap();

    assert_eq!(
        engine.note_body(&r_rec).unwrap(),
        "Private: it made me think about my mother."
    );
    assert_eq!(
        engine.note_body(&v_rec).unwrap(),
        "Public: a patient, generational novel."
    );
    assert_eq!(r_rec.reading_id, v_rec.reading_id);
}

#[tokio::test]
async fn the_vault_file_records_the_kind_and_the_reading() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;
    let note = engine.open_reflection(book, None).await.unwrap();
    let record = engine.storage().get_note(note.id).await.unwrap().unwrap();

    let content = std::fs::read_to_string(&note.file).unwrap();
    assert!(content.contains("kind: reflection"), "{content}");
    assert!(
        content.contains(&format!("reading: {}", record.reading_id.unwrap())),
        "{content}"
    );
    assert!(content.contains("book-title: \"Middlemarch\""), "{content}");
}

#[tokio::test]
async fn wikilinks_from_a_reflection_land_in_the_graph_and_back_resolve() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;

    let reflection = engine.open_reflection(book, None).await.unwrap();
    let record = engine
        .storage()
        .get_note(reflection.id)
        .await
        .unwrap()
        .unwrap();
    engine
        .update_note_body(
            &record,
            "Rhymes with [[Vocation]] and with [[Reflection: 1Q84]].",
        )
        .await
        .unwrap();

    let links = engine.storage().note_links(reflection.id).await.unwrap();
    assert_eq!(links.len(), 2);
    assert!(links.iter().all(|(_, to)| to.is_none()), "both dangle yet");

    let han = engine
        .create_note(NewNoteInput {
            kind: NoteKind::Note,
            title: Some("Vocation".into()),
            body: "The idea of a calling.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let links = engine.storage().note_links(reflection.id).await.unwrap();
    assert_eq!(
        links.iter().find(|(t, _)| t == "Vocation").unwrap().1,
        Some(han.id)
    );

    let other = engine
        .save_book(&Book {
            title: Some("1Q84".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    let other_reflection = engine.open_reflection(other, None).await.unwrap();
    assert_eq!(other_reflection.title, "Reflection: 1Q84");
    let links = engine.storage().note_links(reflection.id).await.unwrap();
    assert_eq!(
        links
            .iter()
            .find(|(t, _)| t == "Reflection: 1Q84")
            .unwrap()
            .1,
        Some(other_reflection.id)
    );
}

#[tokio::test]
async fn rewriting_a_body_replaces_its_edges_rather_than_adding_to_them() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;
    let note = engine.open_reflection(book, None).await.unwrap();
    let record = engine.storage().get_note(note.id).await.unwrap().unwrap();

    engine
        .update_note_body(&record, "See [[Vocation]] and [[Reform]].")
        .await
        .unwrap();
    assert_eq!(engine.storage().note_links(note.id).await.unwrap().len(), 2);

    engine
        .update_note_body(&record, "See [[Vocation]].")
        .await
        .unwrap();
    let links = engine.storage().note_links(note.id).await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].0, "Vocation");
}

#[tokio::test]
async fn a_reflection_is_searchable() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;
    let note = engine.open_reflection(book, None).await.unwrap();
    let record = engine.storage().get_note(note.id).await.unwrap().unwrap();
    engine
        .update_note_body(&record, "The register shifts at the boarding house.")
        .await
        .unwrap();

    let hits = engine
        .search_marks("boarding", Some(bookmole::SearchSource::Note), None, 10)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].as_note().unwrap().id, note.id);
}

#[tokio::test]
async fn citations_survive_a_device_refresh_and_cascade_on_delete() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;
    let hid = engine
        .storage()
        .insert_highlight(
            book,
            &highlight(
                "We are all of us born in moral stupidity",
                "2026-01-05 21:14:08",
            ),
        )
        .await
        .unwrap()
        .expect("newly inserted");

    let review = engine.open_review(book, None).await.unwrap();
    engine.cite(review.id, hid).await.unwrap();
    engine.cite(review.id, hid).await.unwrap();
    assert_eq!(engine.citations_for(review.id).await.unwrap().len(), 1);

    let mut refreshed = highlight(
        "We are all of us born in moral stupidity",
        "2026-01-05 21:14:08",
    );
    refreshed.note = Some("added on the device".into());
    refreshed.color = Some("blue".into());
    assert!(
        engine
            .storage()
            .refresh_device_fields(book, &refreshed)
            .await
            .unwrap()
    );

    let cited = engine.citations_for(review.id).await.unwrap();
    assert_eq!(cited.len(), 1, "the citation is by id, so it holds");
    assert_eq!(cited[0].id, hid);
    assert_eq!(cited[0].origin_note.as_deref(), Some("added on the device"));

    engine.delete_book(book).await.unwrap();
    assert!(
        engine.citations_for(review.id).await.unwrap().is_empty(),
        "citations must cascade rather than dangle"
    );
    assert!(
        engine
            .storage()
            .get_note(review.id)
            .await
            .unwrap()
            .is_some(),
        "the review itself survives its book, as every note does"
    );
}

#[tokio::test]
async fn only_a_review_can_be_rated() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;
    let reflection = engine.open_reflection(book, None).await.unwrap();
    let review = engine.open_review(book, None).await.unwrap();

    assert!(matches!(
        engine.set_rating(reflection.id, 4.0).await,
        Err(EngineError::InvalidInput(_))
    ));
    engine.set_rating(review.id, 4.0).await.unwrap();
    assert_eq!(engine.goodreads_rating(review.id).await.unwrap(), Some(4.0));
}

#[tokio::test]
async fn an_unmapped_value_reports_rather_than_rounds() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;
    let review = engine.open_review(book, None).await.unwrap();

    engine.set_rating(review.id, 4.5).await.unwrap();
    assert_eq!(engine.goodreads_rating(review.id).await.unwrap(), Some(4.5));

    let tens = engine
        .put_rating_scale("tens", 0.0, 10.0, 1.0)
        .await
        .unwrap();
    engine.set_rating(review.id, 9.0).await.unwrap();
    match engine.goodreads_rating(review.id).await {
        Err(EngineError::UnmappedRating { value, scale }) => {
            assert_eq!(value, 9.0);
            assert_eq!(scale, "tens");
        }
        other => panic!("expected an unmapped-rating report, got {other:?}"),
    }

    engine.map_rating(&tens, 9.0, 4.5).await.unwrap();
    assert_eq!(engine.goodreads_rating(review.id).await.unwrap(), Some(4.5));
    engine.map_rating(&tens, 9.0, 4.75).await.unwrap();
    assert_eq!(
        engine.goodreads_rating(review.id).await.unwrap(),
        Some(4.75)
    );

    let stored = engine
        .storage()
        .review_rating(review.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.value, 9.0, "the raw value is what round-trips");
}

#[tokio::test]
async fn an_unrated_review_is_not_an_error() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;
    let review = engine.open_review(book, None).await.unwrap();
    assert_eq!(engine.goodreads_rating(review.id).await.unwrap(), None);
}

#[tokio::test]
async fn a_rating_must_sit_on_the_scale() {
    let (_tmp, engine) = engine().await;
    let book = seeded(&engine).await;
    let review = engine.open_review(book, None).await.unwrap();

    assert!(matches!(
        engine.set_rating(review.id, 4.3).await,
        Err(EngineError::InvalidInput(_))
    ));
    assert!(matches!(
        engine.set_rating(review.id, 7.0).await,
        Err(EngineError::InvalidInput(_))
    ));

    engine
        .storage()
        .put_rating_scale("tenths", 0.0, 10.0, 0.1)
        .await
        .unwrap();
    engine.set_rating(review.id, 7.3).await.unwrap();
    let stored = engine
        .storage()
        .review_rating(review.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.value, 7.3);
    assert_eq!(stored.scale.name, "tenths");
}
