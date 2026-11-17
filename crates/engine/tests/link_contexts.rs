//! `Engine::link_contexts` — a note's links read with the sentence they sit in
//! and the other note's book (proposal 8, model A).
//!
//! Every assertion is on values. The bodies here are fixtures, not anybody's
//! reading, but the habit is the rule's: nothing prints a paragraph.

mod common;

use bookmole::{LINK_PARAGRAPH_CAP, LinkDirection, NewNoteInput, NoteKind};
use common::{engine, seed_book};

async fn note(engine: &bookmole::Engine, book_id: Option<i64>, title: &str, body: &str) -> i64 {
    engine
        .create_note(NewNoteInput {
            book_id,
            kind: NoteKind::Note,
            title: Some(title.into()),
            body: body.into(),
            ..Default::default()
        })
        .await
        .expect("create note")
        .id
}

/// The scenario from the brief: two books, A links B, C links A under an
/// alias, A writes a forward reference and links itself.
#[tokio::test]
async fn both_directions_carry_their_book_and_the_incoming_paragraph() {
    let (_tmp, engine) = engine().await;
    let middlemarch = seed_book(&engine, "Middlemarch").await;
    let millionaires = seed_book(&engine, "Silas Marner").await;

    let b = note(&engine, Some(millionaires), "Beta", "Nothing outward.").await;
    let a = note(
        &engine,
        Some(middlemarch),
        "Alpha",
        "Opening line.\n\nThis sits beside [[Beta]] on purpose.\nStill the same paragraph.\n\n\
         Later, [[Not written yet]] and a nod to [[Alpha]] itself.",
    )
    .await;
    let c = note(
        &engine,
        Some(millionaires),
        "Gamma",
        "Preamble.\n\nGamma recalls [[Alpha|that note]] here.\n\nCoda.",
    )
    .await;

    let rows = engine.link_contexts(a).await.unwrap();
    let shape: Vec<_> = rows
        .iter()
        .map(|r| {
            (
                r.direction,
                r.target_title.as_str(),
                r.note.as_ref().map(|n| n.id),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            (LinkDirection::Incoming, "Alpha", Some(c)),
            (LinkDirection::Outgoing, "Beta", Some(b)),
            (LinkDirection::Outgoing, "Not written yet", None),
        ],
        "incoming first, then outgoing in body order; the self-link is in neither"
    );

    // The incoming row: C's book and the paragraph holding the link.
    let inc = &rows[0];
    assert_eq!(inc.book.as_ref().and_then(|b| b.id), Some(millionaires));
    let p = inc
        .paragraph
        .as_ref()
        .expect("an incoming link has its paragraph");
    assert_eq!(p.text, "Gamma recalls [[Alpha|that note]] here.");
    assert_eq!(
        &p.text[p.link_range.0..p.link_range.1],
        "[[Alpha|that note]]"
    );
    assert!(!p.truncated && !p.truncated_start);

    // Outgoing rows: the target's book, no paragraph (it is in A's own body).
    assert_eq!(rows[1].book.as_ref().and_then(|b| b.id), Some(millionaires));
    assert!(rows[1].paragraph.is_none());
    assert!(rows[2].book.is_none() && rows[2].paragraph.is_none());

    // From B's side, A is the incoming row, with A's book and a two-line
    // paragraph.
    let rows = engine.link_contexts(b).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].direction, LinkDirection::Incoming);
    assert_eq!(rows[0].note.as_ref().map(|n| n.id), Some(a));
    assert_eq!(rows[0].book.as_ref().and_then(|b| b.id), Some(middlemarch));
    let p = rows[0].paragraph.as_ref().unwrap();
    assert_eq!(
        p.text,
        "This sits beside [[Beta]] on purpose.\nStill the same paragraph."
    );
    assert_eq!(p.link_range, (17, 25));

    // A note with no links has no rows, rather than an error.
    let lone = note(&engine, None, "Lone", "No links at all.").await;
    assert!(engine.link_contexts(lone).await.unwrap().is_empty());
}

/// A long paragraph comes back cut around the link and flagged; the newest-kept
/// source is the first incoming row.
#[tokio::test]
async fn a_long_incoming_paragraph_is_cut_and_the_newest_source_leads() {
    let (_tmp, engine) = engine().await;
    let target = note(&engine, None, "Target", "Here.").await;
    let short = note(&engine, None, "Short", "See [[Target]].").await;
    let filler = "word ".repeat(200);
    let long = note(
        &engine,
        None,
        "Long",
        &format!("{filler}then [[Target]] and {filler}"),
    )
    .await;

    let rows = engine.link_contexts(target).await.unwrap();
    let ids: Vec<_> = rows.iter().map(|r| r.note.as_ref().unwrap().id).collect();
    // Created in this order within the test, so `long` is never older; the id
    // breaks a same-second tie the same way.
    assert_eq!(ids, vec![long, short]);

    let p = rows[0].paragraph.as_ref().unwrap();
    assert!(p.truncated && p.truncated_start);
    assert!(p.text.len() <= LINK_PARAGRAPH_CAP);
    assert_eq!(&p.text[p.link_range.0..p.link_range.1], "[[Target]]");

    let p = rows[1].paragraph.as_ref().unwrap();
    assert!(!p.truncated);
    assert_eq!(p.link_range, (4, 14));
}
