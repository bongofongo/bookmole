//! Tests for walking the note graph inbound and outbound.

mod common;

use bookmole::{NewNoteInput, NoteKind};
use common::{engine, seed_book};

#[tokio::test]
async fn inbound_and_outbound_are_different_sets_and_a_self_link_counts_once() {
    let (_tmp, engine) = engine().await;
    let middlemarch = seed_book(&engine, "Middlemarch").await;
    let millionaires = seed_book(&engine, "Silas Marner").await;

    let a = engine.open_reflection(middlemarch, None).await.unwrap();
    let b = engine.open_reflection(millionaires, None).await.unwrap();
    let a_rec = engine.storage().get_note(a.id).await.unwrap().unwrap();
    let b_rec = engine.storage().get_note(b.id).await.unwrap().unwrap();

    engine
        .update_note_body(
            &a_rec,
            &format!(
                "Same preoccupations as [[{}]]. Restating [[{}]] to see it again.",
                b.title, a.title
            ),
        )
        .await
        .unwrap();
    engine
        .update_note_body(&b_rec, &format!("The later book: [[{}]].", a.title))
        .await
        .unwrap();

    let out: Vec<_> = engine
        .outgoing_links(a.id)
        .await
        .unwrap()
        .into_iter()
        .map(|l| (l.target_title, l.to.map(|n| n.id)))
        .collect();
    assert_eq!(
        out,
        vec![(b.title.clone(), Some(b.id)), (a.title.clone(), Some(a.id)),],
        "outgoing follows the body, in the order the body wrote it"
    );

    let inbound: Vec<i64> = engine
        .backlinks(a.id)
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(inbound.len(), 2, "B links here, and so does A itself");
    assert!(inbound.contains(&b.id));
    assert_eq!(
        inbound.iter().filter(|id| **id == a.id).count(),
        1,
        "a self-link is one edge, not two"
    );

    assert_eq!(engine.backlinks(b.id).await.unwrap().len(), 1);
    assert_eq!(engine.outgoing_links(b.id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_forward_reference_resolves_when_its_target_is_written() {
    let (_tmp, engine) = engine().await;
    let middlemarch = seed_book(&engine, "Middlemarch").await;

    let early = engine
        .create_note(NewNoteInput {
            kind: NoteKind::Note,
            title: Some("Diaspora, generally".into()),
            body: "Whatever I end up saying in [[Reflection: Middlemarch]] starts here.".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let out = engine.outgoing_links(early.id).await.unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].target_title, "Reflection: Middlemarch");
    assert!(out[0].to.is_none(), "nothing to resolve to yet");

    let reflection = engine.open_reflection(middlemarch, None).await.unwrap();
    assert_eq!(reflection.title, "Reflection: Middlemarch");

    let inbound: Vec<i64> = engine
        .backlinks(reflection.id)
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(
        inbound,
        vec![early.id],
        "a plain `WHERE to_note = ?` has to find the forward reference"
    );
    let out = engine.outgoing_links(early.id).await.unwrap();
    assert_eq!(
        out[0].to.as_ref().map(|n| n.id),
        Some(reflection.id),
        "and both directions must agree the edge resolved"
    );
}

#[tokio::test]
async fn deleting_the_target_leaves_the_link_as_text() {
    let (_tmp, engine) = engine().await;
    let middlemarch = seed_book(&engine, "Middlemarch").await;

    let target = engine
        .create_note(NewNoteInput {
            kind: NoteKind::Note,
            title: Some("Vocation".into()),
            body: "A calling, and what it costs.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let source = engine
        .create_note(NewNoteInput {
            book_id: Some(middlemarch),
            kind: NoteKind::Note,
            title: Some("Dorothea".into()),
            body: "Her whole life is [[Vocation]].".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(engine.backlinks(target.id).await.unwrap().len(), 1);

    let target_rec = engine.storage().get_note(target.id).await.unwrap().unwrap();
    engine.delete_note(&target_rec).await.unwrap();

    let out = engine.outgoing_links(source.id).await.unwrap();
    assert_eq!(out.len(), 1, "the edge is the source's, and it stays");
    assert_eq!(out[0].target_title, "Vocation");
    assert!(out[0].to.is_none(), "it went back to being text");

    let again = engine
        .create_note(NewNoteInput {
            kind: NoteKind::Note,
            title: Some("Vocation".into()),
            body: "Second attempt at the same idea.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let inbound: Vec<i64> = engine
        .backlinks(again.id)
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(inbound, vec![source.id]);
}

#[tokio::test]
async fn a_title_shared_by_two_notes_is_where_back_resolution_stops() {
    let (_tmp, engine) = engine().await;

    let first = engine
        .create_note(NewNoteInput {
            kind: NoteKind::Note,
            title: Some("Vocation".into()),
            body: "One reading of it.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let second = engine
        .create_note(NewNoteInput {
            kind: NoteKind::Note,
            title: Some("Vocation".into()),
            body: "Another, under the same name.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let source = engine
        .create_note(NewNoteInput {
            kind: NoteKind::Note,
            title: Some("Dorothea".into()),
            body: "Her whole life is [[Vocation]].".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let out = engine.outgoing_links(source.id).await.unwrap();
    assert_eq!(
        out[0].to.as_ref().map(|n| n.id),
        Some(first.id),
        "an ambiguous title resolves to one of them, and it is the older"
    );

    let first_rec = engine.storage().get_note(first.id).await.unwrap().unwrap();
    engine.delete_note(&first_rec).await.unwrap();

    let out = engine.outgoing_links(source.id).await.unwrap();
    assert!(
        out[0].to.is_none(),
        "the edge dangles again even though a note with that title survives"
    );
    assert!(
        engine.backlinks(second.id).await.unwrap().is_empty(),
        "and the surviving namesake must report the same thing, not a link the \
         source no longer claims to have"
    );
}

#[tokio::test]
async fn a_rewritten_body_drops_the_backlink_it_no_longer_writes() {
    let (_tmp, engine) = engine().await;
    let middlemarch = seed_book(&engine, "Middlemarch").await;
    let millionaires = seed_book(&engine, "Silas Marner").await;

    let a = engine.open_reflection(middlemarch, None).await.unwrap();
    let b = engine.open_reflection(millionaires, None).await.unwrap();
    let a_rec = engine.storage().get_note(a.id).await.unwrap().unwrap();

    engine
        .update_note_body(&a_rec, &format!("Twenty years earlier: [[{}]].", b.title))
        .await
        .unwrap();
    assert_eq!(engine.backlinks(b.id).await.unwrap().len(), 1);

    engine
        .update_note_body(&a_rec, "On reflection, they are not much alike.")
        .await
        .unwrap();
    assert!(
        engine.backlinks(b.id).await.unwrap().is_empty(),
        "a link the user deleted has to leave the inbound side too"
    );
    assert!(engine.outgoing_links(a.id).await.unwrap().is_empty());
}
