//! A highlight's alternative annotations: set aside by a fold, listed, promoted and removed.

mod common;

use bookmole::{Engine, EngineError};

/// The same passage on two books, annotated differently, merged into `keep`'s book.
async fn two_annotations(engine: &Engine) -> (i64, i64) {
    let book = common::seed_book(engine, "Middlemarch").await;
    let again = common::seed_book(engine, "Middlemarch (again)").await;
    let h = common::highlight("a phrase worth keeping", "2026-01-05 21:14:08");
    let keep = engine
        .storage()
        .insert_highlight(book, &h)
        .await
        .unwrap()
        .unwrap();
    let dup = engine
        .storage()
        .insert_highlight(again, &h)
        .await
        .unwrap()
        .unwrap();
    engine.set_annotation(keep, Some("mine")).await.unwrap();
    engine.set_annotation(dup, Some("theirs")).await.unwrap();
    engine.merge_books(again, book).await.unwrap();
    (book, keep)
}

async fn texts(engine: &Engine, highlight: i64) -> Vec<String> {
    engine
        .annotation_alternatives(highlight)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.text)
        .collect()
}

async fn shown(engine: &Engine, highlight: i64) -> Option<String> {
    engine
        .get_highlight(highlight)
        .await
        .unwrap()
        .unwrap()
        .annotation
}

#[tokio::test]
async fn a_merge_sets_the_other_annotation_aside() {
    let (_tmp, engine) = common::engine().await;
    let (book, keep) = two_annotations(&engine).await;
    assert_eq!(shown(&engine, keep).await.as_deref(), Some("mine"));
    assert_eq!(texts(&engine, keep).await, ["theirs"]);

    let whole_book = engine.book_annotation_alternatives(book).await.unwrap();
    assert_eq!(whole_book.len(), 1);
    assert_eq!(whole_book[0].highlight_id, keep);
    assert_eq!(whole_book[0].text, "theirs");
}

#[tokio::test]
async fn a_book_read_covers_every_highlight_and_no_other_book() {
    let (_tmp, engine) = common::engine().await;
    let (book, keep) = two_annotations(&engine).await;
    let other = common::seed_book(&engine, "Silas Marner").await;
    let lone = engine
        .storage()
        .insert_highlight(
            other,
            &common::highlight("elsewhere", "2026-02-01 10:00:00"),
        )
        .await
        .unwrap()
        .unwrap();
    engine.set_annotation(lone, Some("shown")).await.unwrap();
    assert!(
        engine
            .book_annotation_alternatives(other)
            .await
            .unwrap()
            .is_empty()
    );
    let second = engine
        .storage()
        .insert_highlight(book, &common::highlight("a second", "2026-03-01 10:00:00"))
        .await
        .unwrap()
        .unwrap();
    engine.set_annotation(second, Some("one")).await.unwrap();
    engine
        .promote_annotation_alternative(keep, "theirs")
        .await
        .unwrap();
    let ids: Vec<i64> = engine
        .book_annotation_alternatives(book)
        .await
        .unwrap()
        .iter()
        .map(|a| a.highlight_id)
        .collect();
    assert_eq!(ids, [keep]);
}

#[tokio::test]
async fn promoting_swaps_and_loses_nothing() {
    let (_tmp, engine) = common::engine().await;
    let (_book, keep) = two_annotations(&engine).await;

    engine
        .promote_annotation_alternative(keep, "theirs")
        .await
        .unwrap();
    assert_eq!(shown(&engine, keep).await.as_deref(), Some("theirs"));
    assert_eq!(texts(&engine, keep).await, ["mine"]);

    engine
        .promote_annotation_alternative(keep, "mine")
        .await
        .unwrap();
    assert_eq!(shown(&engine, keep).await.as_deref(), Some("mine"));
    assert_eq!(texts(&engine, keep).await, ["theirs"]);

    // With nothing shown, a promotion sets nothing aside.
    engine.set_annotation(keep, None).await.unwrap();
    engine
        .promote_annotation_alternative(keep, "theirs")
        .await
        .unwrap();
    assert_eq!(shown(&engine, keep).await.as_deref(), Some("theirs"));
    assert!(texts(&engine, keep).await.is_empty());
}

#[tokio::test]
async fn removing_is_idempotent_and_both_verbs_check_the_highlight() {
    let (_tmp, engine) = common::engine().await;
    let (_book, keep) = two_annotations(&engine).await;

    engine
        .remove_annotation_alternative(keep, "theirs")
        .await
        .unwrap();
    assert!(texts(&engine, keep).await.is_empty());
    engine
        .remove_annotation_alternative(keep, "theirs")
        .await
        .unwrap();
    assert_eq!(shown(&engine, keep).await.as_deref(), Some("mine"));

    let missing = keep + 1_000;
    for err in [
        engine.annotation_alternatives(missing).await.unwrap_err(),
        engine
            .remove_annotation_alternative(missing, "x")
            .await
            .unwrap_err(),
        engine
            .promote_annotation_alternative(missing, "x")
            .await
            .unwrap_err(),
        engine
            .promote_annotation_alternative(keep, "never set aside")
            .await
            .unwrap_err(),
    ] {
        assert!(matches!(err, EngineError::NotFound(_)), "{err:?}");
    }
    assert_eq!(
        shown(&engine, keep).await.as_deref(),
        Some("mine"),
        "a refused promotion changes nothing"
    );
}
