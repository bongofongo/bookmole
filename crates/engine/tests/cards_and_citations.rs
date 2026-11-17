use bookmole::{Engine, EngineError};

mod common;
use common::{engine, highlight, seed_book};

async fn highlights(engine: &Engine, book: i64, texts: &[&str]) -> Vec<i64> {
    let mut ids = Vec::new();
    for (i, text) in texts.iter().enumerate() {
        let when = format!("2026-01-{:02} 09:00:00", i + 1);
        ids.push(
            engine
                .storage()
                .insert_highlight(book, &highlight(text, &when))
                .await
                .unwrap()
                .expect("newly inserted"),
        );
    }
    ids
}

#[tokio::test]
async fn a_card_round_trips_its_book_and_its_passage() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    let hs = highlights(&engine, book, &["middlemarch"]).await;

    assert!(
        engine
            .create_flashcard(book, Some(hs[0]), "middlemarch", Some("Ch 1"))
            .await
            .unwrap(),
        "the first card is new"
    );

    for card in [
        engine.list_flashcards(true).await.unwrap().remove(0),
        engine
            .list_flashcards_for_book(book)
            .await
            .unwrap()
            .remove(0),
    ] {
        assert_eq!(card.book_id, book);
        assert_eq!(card.highlight_id, Some(hs[0]));
        assert_eq!(card.book_title, "Middlemarch");
        assert_eq!(card.context.as_deref(), Some("Ch 1"));
    }
}

#[tokio::test]
async fn a_repeat_word_reports_not_new_and_leaves_the_first_card_alone() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    let hs = highlights(&engine, book, &["middlemarch", "ardent"]).await;

    assert!(
        engine
            .create_flashcard(book, Some(hs[0]), "middlemarch", Some("first"))
            .await
            .unwrap()
    );
    assert!(
        !engine
            .create_flashcard(book, Some(hs[1]), "middlemarch", Some("second"))
            .await
            .unwrap(),
        "the second is not new"
    );

    let cards = engine.list_flashcards_for_book(book).await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].context.as_deref(), Some("first"));
    assert_eq!(
        cards[0].highlight_id,
        Some(hs[0]),
        "a repeat capture must not move the card onto another passage"
    );
}

#[tokio::test]
async fn a_card_cannot_be_anchored_to_another_books_passage() {
    let (_tmp, engine) = engine().await;
    let middlemarch = seed_book(&engine, "Middlemarch").await;
    let station = seed_book(&engine, "Station Eleven").await;
    let elsewhere = highlights(&engine, station, &["survival"]).await;

    assert!(matches!(
        engine
            .create_flashcard(middlemarch, Some(elsewhere[0]), "survival", None)
            .await,
        Err(EngineError::InvalidInput(_))
    ));
    assert!(
        engine
            .list_flashcards_for_book(middlemarch)
            .await
            .unwrap()
            .is_empty(),
        "a refused card writes nothing"
    );

    assert!(matches!(
        engine
            .create_flashcard(middlemarch, Some(9_999), "ghost", None)
            .await,
        Err(EngineError::NotFound(_))
    ));
    assert!(matches!(
        engine.create_flashcard(9_999, None, "ghost", None).await,
        Err(EngineError::NotFound(_))
    ));
    assert!(matches!(
        engine
            .create_flashcard(middlemarch, None, "   ", None)
            .await,
        Err(EngineError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn a_card_needs_no_passage_and_its_word_is_trimmed() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;

    assert!(
        engine
            .create_flashcard(book, None, "  mot  ", None)
            .await
            .unwrap()
    );
    assert!(
        !engine
            .create_flashcard(book, None, "mot", None)
            .await
            .unwrap(),
        "the trim is what makes this the same card"
    );

    let cards = engine.list_flashcards_for_book(book).await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].word, "mot");
    assert_eq!(cards[0].highlight_id, None);
}

#[tokio::test]
async fn the_batch_and_the_single_note_call_cannot_disagree() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    let hs = highlights(
        &engine,
        book,
        &[
            "born in moral stupidity",
            "middlemarch",
            "a woman's lot",
            "home",
        ],
    )
    .await;

    let reflection = engine.open_reflection(book, None).await.unwrap();
    let review = engine.open_review(book, None).await.unwrap();
    // Cited out of reading order on purpose: the reply is ordered by the book, not by citation time.
    for h in [hs[3], hs[0], hs[2]] {
        engine.cite(reflection.id, h).await.unwrap();
    }
    engine.cite(review.id, hs[1]).await.unwrap();

    let asked = [review.id, reflection.id, 9_999, reflection.id];
    let batch = engine.citations_for_notes(&asked).await.unwrap();
    assert_eq!(batch.len(), asked.len());
    for (row, note_id) in batch.iter().zip(asked) {
        assert_eq!(row.note_id, note_id);
        let alone: Vec<i64> = engine
            .citations_for(note_id)
            .await
            .unwrap()
            .into_iter()
            .map(|h| h.id)
            .collect();
        assert_eq!(row.highlight_ids, alone, "note {note_id}");
    }
}

#[tokio::test]
async fn every_requested_note_gets_a_row_in_the_order_asked() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    let hs = highlights(&engine, book, &["middlemarch"]).await;

    let cites = engine.open_reflection(book, None).await.unwrap();
    let silent = engine.open_review(book, None).await.unwrap();
    engine.cite(cites.id, hs[0]).await.unwrap();

    let asked = [silent.id, 4_242, cites.id, silent.id];
    let batch = engine.citations_for_notes(&asked).await.unwrap();

    assert_eq!(
        batch.iter().map(|r| r.note_id).collect::<Vec<_>>(),
        asked.to_vec(),
        "duplicates included, order preserved"
    );
    assert!(batch[0].highlight_ids.is_empty(), "cites nothing");
    assert!(batch[1].highlight_ids.is_empty(), "not a note at all");
    assert_eq!(batch[2].highlight_ids, vec![hs[0]]);
    assert_eq!(batch[3].highlight_ids, Vec::<i64>::new());

    assert!(
        engine.citations_for_notes(&[]).await.unwrap().is_empty(),
        "nothing asked, nothing answered"
    );
}

/// Crosses the parameter-chunk boundary (1,203 ids) to check no row is lost, duplicated or
/// reordered; a SQLite that binds 32,766 parameters passes even without the chunk.
#[tokio::test]
async fn more_ids_than_one_statement_can_bind() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    let hs = highlights(&engine, book, &["middlemarch"]).await;

    let real = engine.open_reflection(book, None).await.unwrap();
    engine.cite(real.id, hs[0]).await.unwrap();

    let mut asked: Vec<i64> = (100_000..101_202).collect();
    asked.insert(700, real.id);

    let batch = engine.citations_for_notes(&asked).await.unwrap();
    assert_eq!(batch.len(), asked.len());
    assert_eq!(batch.iter().map(|r| r.note_id).collect::<Vec<_>>(), asked);
    assert_eq!(batch[700].highlight_ids, vec![hs[0]]);
    assert_eq!(
        batch.iter().filter(|r| !r.highlight_ids.is_empty()).count(),
        1,
    );
}

#[tokio::test]
async fn the_batch_follows_the_citation_rather_than_a_copy_of_it() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    let hs = highlights(&engine, book, &["middlemarch", "ardent"]).await;
    let note = engine.open_reflection(book, None).await.unwrap();
    engine.cite(note.id, hs[0]).await.unwrap();
    engine.cite(note.id, hs[1]).await.unwrap();

    assert_eq!(
        engine.citations_for_notes(&[note.id]).await.unwrap()[0]
            .highlight_ids
            .len(),
        2
    );

    assert!(engine.uncite(note.id, hs[0]).await.unwrap());
    assert_eq!(
        engine.citations_for_notes(&[note.id]).await.unwrap()[0].highlight_ids,
        vec![hs[1]]
    );

    engine.delete_book(book).await.unwrap();
    let after = engine.citations_for_notes(&[note.id]).await.unwrap();
    assert_eq!(after.len(), 1, "still one row per id asked");
    assert!(after[0].highlight_ids.is_empty());
}
