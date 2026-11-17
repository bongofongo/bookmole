//! `resurface_note`: one older note for the reader's day.
//!
//! Every note a test creates is stamped *now*, so the tests move `today`
//! instead of the notes: a date a month out makes every note old enough, and
//! the real date makes every note too fresh. That keeps the tests off any
//! clock-mocking seam the engine does not otherwise need.

mod common;

use bookmole::{NewNoteInput, ResurfaceReason};
use common::{engine, seed_book};
use time::{Date, Duration, OffsetDateTime};

fn later() -> Date {
    OffsetDateTime::now_utc().date() + Duration::days(30)
}

async fn note(engine: &bookmole::Engine, book_id: Option<i64>, body: &str) -> i64 {
    engine
        .create_note(NewNoteInput {
            book_id,
            body: body.into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn an_empty_notebook_offers_nothing() {
    let (_tmp, engine) = engine().await;
    assert!(engine.resurface_note(later()).await.unwrap().is_none());
}

#[tokio::test]
async fn a_note_kept_this_week_does_not_come_back() {
    let (_tmp, engine) = engine().await;
    note(&engine, None, "Fresh thought.").await;
    let today = OffsetDateTime::now_utc().date();
    assert!(engine.resurface_note(today).await.unwrap().is_none());
    // Six days on it is still resting; eight days on it is not.
    assert!(
        engine
            .resurface_note(today + Duration::days(6))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        engine
            .resurface_note(today + Duration::days(8))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn the_same_day_gives_the_same_note_and_the_days_move_through_the_notebook() {
    let (_tmp, engine) = engine().await;
    for i in 0..12 {
        note(&engine, None, &format!("Loose note {i}.")).await;
    }
    let day = later();
    let first = engine.resurface_note(day).await.unwrap().unwrap();
    let again = engine.resurface_note(day).await.unwrap().unwrap();
    assert_eq!(first.note.id, again.note.id, "one note all day");
    assert_eq!(first.reason, ResurfaceReason::Notebook);

    let mut seen = std::collections::HashSet::new();
    for d in 0..30 {
        let r = engine
            .resurface_note(day + Duration::days(d))
            .await
            .unwrap()
            .unwrap();
        seen.insert(r.note.id);
    }
    assert!(
        seen.len() > 3,
        "a month of days should not keep returning one or two notes: {seen:?}"
    );
}

#[tokio::test]
async fn a_note_touching_an_open_book_wins_over_the_rest() {
    let (_tmp, engine) = engine().await;
    let open = seed_book(&engine, "Middlemarch").await;
    let shelved = seed_book(&engine, "Kokoro").await;
    engine
        .storage()
        .update_progress(open, Some(40), None)
        .await
        .unwrap();

    for i in 0..8 {
        note(&engine, Some(shelved), &format!("Kokoro note {i}.")).await;
    }
    let near = note(&engine, Some(open), "Dorothea on the ferry.").await;

    for d in 0..14 {
        let r = engine
            .resurface_note(later() + Duration::days(d))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.note.id, near, "the open book's note, every day");
        assert_eq!(r.reason, ResurfaceReason::OpenBook);
    }

    // Finishing the book closes the reading, and the preference with it.
    engine
        .storage()
        .update_progress(open, None, Some(true))
        .await
        .unwrap();
    let r = engine.resurface_note(later()).await.unwrap().unwrap();
    assert_eq!(r.reason, ResurfaceReason::Notebook);
}

#[tokio::test]
async fn a_free_note_linked_to_the_open_books_note_counts_as_connected() {
    let (_tmp, engine) = engine().await;
    let open = seed_book(&engine, "Middlemarch").await;
    let shelved = seed_book(&engine, "Kokoro").await;
    engine
        .storage()
        .update_progress(open, Some(40), None)
        .await
        .unwrap();
    for i in 0..6 {
        note(&engine, Some(shelved), &format!("Kokoro note {i}.")).await;
    }
    let anchor = engine
        .create_note(NewNoteInput {
            book_id: Some(open),
            title: Some("Ferry".into()),
            body: "Dorothea on the ferry.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let free = note(
        &engine,
        None,
        &format!("Exile, again: [[{}]].", anchor.title),
    )
    .await;

    let mut picked = std::collections::HashSet::new();
    for d in 0..30 {
        let r = engine
            .resurface_note(later() + Duration::days(d))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.reason, ResurfaceReason::OpenBook);
        picked.insert(r.note.id);
    }
    assert!(picked.iter().all(|id| *id == anchor.id || *id == free));
    assert!(
        picked.contains(&free),
        "the linked free note is in the near tier"
    );
}

// ---------------------------------------------------------------------------
// `resurface_passage`: the note's twin, one older passage for the day.
// Same rule, so the same four questions — and one of its own: a blank passage
// is never what comes back.
// ---------------------------------------------------------------------------

async fn passage(engine: &bookmole::Engine, book_id: i64, text: &str) -> i64 {
    engine
        .storage()
        .insert_highlight(book_id, &common::highlight(text, "2026-01-02 10:00:00"))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn no_passages_offers_nothing() {
    let (_tmp, engine) = engine().await;
    seed_book(&engine, "Kokoro").await;
    assert!(engine.resurface_passage(later()).await.unwrap().is_none());
}

#[tokio::test]
async fn a_passage_stored_this_week_rests_like_a_note() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Kokoro").await;
    passage(&engine, book, "The heart is a lonely place.").await;
    let today = OffsetDateTime::now_utc().date();
    assert!(engine.resurface_passage(today).await.unwrap().is_none());
    assert!(
        engine
            .resurface_passage(today + Duration::days(6))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        engine
            .resurface_passage(today + Duration::days(8))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn the_same_day_gives_the_same_passage_and_the_days_move() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Kokoro").await;
    for i in 0..8 {
        passage(&engine, book, &format!("Passage {i}.")).await;
    }
    let day = later();
    let first = engine.resurface_passage(day).await.unwrap().unwrap();
    let again = engine.resurface_passage(day).await.unwrap().unwrap();
    assert_eq!(first.highlight.id, again.highlight.id);
    let mut seen = std::collections::HashSet::new();
    for d in 0..14 {
        let r = engine
            .resurface_passage(day + Duration::days(d))
            .await
            .unwrap()
            .unwrap();
        seen.insert(r.highlight.id);
    }
    assert!(seen.len() > 1, "a fortnight offered one passage: {seen:?}");
}

#[tokio::test]
async fn the_open_books_passage_wins_and_finishing_releases_it() {
    let (_tmp, engine) = engine().await;
    let open = seed_book(&engine, "Middlemarch").await;
    let shelved = seed_book(&engine, "Kokoro").await;
    engine
        .storage()
        .update_progress(open, Some(40), None)
        .await
        .unwrap();
    for i in 0..8 {
        passage(&engine, shelved, &format!("Kokoro passage {i}.")).await;
    }
    let near = passage(&engine, open, "Dorothea on the ferry.").await;
    for d in 0..14 {
        let r = engine
            .resurface_passage(later() + Duration::days(d))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.highlight.id, near, "the open book's passage, every day");
        assert_eq!(r.reason, ResurfaceReason::OpenBook);
    }
    engine
        .storage()
        .update_progress(open, None, Some(true))
        .await
        .unwrap();
    let r = engine.resurface_passage(later()).await.unwrap().unwrap();
    assert_eq!(r.reason, ResurfaceReason::Notebook);
}

#[tokio::test]
async fn a_blank_passage_is_never_offered() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "Kokoro").await;
    passage(&engine, book, "   ").await;
    assert!(engine.resurface_passage(later()).await.unwrap().is_none());
    let kept = passage(&engine, book, "Words.").await;
    for d in 0..7 {
        let r = engine
            .resurface_passage(later() + Duration::days(d))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r.highlight.id, kept);
    }
}
