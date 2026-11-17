mod common;

use bookmole::{Engine, EngineConfig, NewNoteInput, NoteKind, NoteRowQuery, ReadMark};

async fn engine() -> (tempfile::TempDir, Engine) {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("database")).unwrap();
    let config = EngineConfig {
        db_url: format!(
            "sqlite://{}?mode=rwc",
            tmp.path().join("database/app.db").display()
        ),
        images_dir: tmp.path().join("database/images"),
        files_dir: tmp.path().join("database/files"),
        vault_dir: tmp.path().join("vault"),
        log_dir: tmp.path().join("logs"),
        exports_dir: tmp.path().join("exports"),
        google_api_key: None,
        calibre_bin_dir: None,
    };
    let engine = Engine::open(config).await.expect("engine opens");
    (tmp, engine)
}

async fn stamp(engine: &Engine, id: i64, at: i64) {
    let pool = sqlx::SqlitePool::connect(engine.db_url()).await.unwrap();
    sqlx::query("UPDATE notes SET created_at = ?1, latest_part_at = ?1 WHERE id = ?2")
        .bind(at)
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
}

async fn note(engine: &Engine, book: Option<i64>, title: &str, body: &str) -> i64 {
    engine
        .create_note(NewNoteInput {
            book_id: book,
            kind: NoteKind::Note,
            title: Some(title.into()),
            body: body.into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn a_passage_by_id_and_faces_in_the_order_asked() {
    let (_tmp, engine) = engine().await;
    let a = common::seed_book(&engine, "Middlemarch").await;
    let b = common::seed_book(&engine, "Kokoro").await;
    let h = engine
        .storage()
        .insert_highlight(a, &common::highlight("x", "2026-01-05 10:00:00"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(engine.get_highlight(h).await.unwrap().unwrap().book_id, a);
    assert!(engine.get_highlight(h + 100).await.unwrap().is_none());
    let faces: Vec<i64> = engine
        .book_faces(&[b, 999, a, b])
        .await
        .unwrap()
        .iter()
        .map(|f| f.id.unwrap())
        .collect();
    assert_eq!(faces, vec![b, a, b], "asked order, a missing id left out");
}

#[tokio::test]
async fn the_cursor_never_skips_two_notes_sharing_a_second() {
    let (_tmp, engine) = engine().await;
    let ids: Vec<i64> = {
        let mut v = Vec::new();
        for (i, t) in ["one", "two", "three", "four"].iter().enumerate() {
            let id = note(&engine, None, t, "body").await;
            // Two pairs, each pair sharing one second.
            stamp(&engine, id, 1_000 + (i as i64 / 2)).await;
            v.push(id);
        }
        v
    };
    let page = |after| NoteRowQuery {
        after,
        limit: 1,
        ..Default::default()
    };
    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let rows = engine.note_rows(&page(after)).await.unwrap();
        let Some(r) = rows.first() else { break };
        seen.push(r.note.id);
        after = Some((r.latest_part_at, r.note.id));
    }
    let expected = vec![ids[3], ids[2], ids[1], ids[0]];
    assert_eq!(
        seen, expected,
        "one row a page walks every note exactly once"
    );
}

#[tokio::test]
async fn a_reads_marks_interleave_and_a_review_row_knows_its_read() {
    let (_tmp, engine) = engine().await;
    let s = engine.storage();
    let book = common::seed_book(&engine, "Middlemarch").await;
    let first = s
        .record_reading(
            book,
            Some(1_767_571_200),
            Some(1_769_299_200),
            "finished",
            "manual",
        )
        .await
        .unwrap();
    s.record_reading(book, Some(1_769_904_000), None, "reading", "manual")
        .await
        .unwrap();
    s.insert_highlight(book, &common::highlight("p1", "2026-01-06 10:00:00"))
        .await
        .unwrap();
    s.insert_highlight(book, &common::highlight("p2", "2026-01-20 10:00:00"))
        .await
        .unwrap();
    s.attribute_highlights(book).await.unwrap();
    let during = note(&engine, Some(book), "during", "a thought").await;
    stamp(&engine, during, 1_767_571_200 + 5 * 86_400).await; // 10 Jan
    let after = note(&engine, Some(book), "after", "later").await;
    stamp(&engine, after, 1_769_904_000 + 86_400).await; // in the reread
    let review = engine.open_review(book, Some(first)).await.unwrap().id;
    stamp(&engine, review, 1_769_299_200).await; // 25 Jan

    let marks = engine.read_marks(first, -1).await.unwrap();
    let kinds: Vec<&str> = marks
        .iter()
        .map(|m| match m {
            ReadMark::Note { .. } => "note",
            ReadMark::Passage { .. } => "passage",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["note", "passage", "note", "passage"],
        "newest first, interleaved"
    );
    assert!(marks.windows(2).all(|w| w[0].at() >= w[1].at()));
    assert_eq!(engine.read_marks(first, 2).await.unwrap().len(), 2);
    assert!(engine.read_marks(9_999, -1).await.is_err());

    let rows = engine
        .note_rows(&NoteRowQuery {
            ids: Some(vec![review, during]),
            limit: -1,
            ..Default::default()
        })
        .await
        .unwrap();
    let review_row = rows.iter().find(|r| r.note.id == review).unwrap();
    let read = review_row.read.unwrap();
    assert_eq!((read.number, read.of), (1, 2), "the first read's review");
    assert!(
        rows.iter()
            .find(|r| r.note.id == during)
            .unwrap()
            .read
            .is_none()
    );
}

#[tokio::test]
async fn note_books_counts_the_linked_notes_of_each_book() {
    let (_tmp, engine) = engine().await;
    let a = common::seed_book(&engine, "Middlemarch").await;
    let b = common::seed_book(&engine, "Kokoro").await;
    note(&engine, Some(a), "Hub", "see [[Spoke]] and [[Nowhere]]").await;
    note(&engine, Some(b), "Spoke", "plain").await;
    note(&engine, Some(a), "Alone", "no links").await;
    note(&engine, Some(b), "Other", "also [[Spoke]]").await;
    let books: Vec<(i64, i64)> = engine
        .note_books()
        .await
        .unwrap()
        .into_iter()
        .map(|(bk, n)| (bk.id.unwrap(), n))
        .collect();
    assert_eq!(books, vec![(b, 2), (a, 1)]);
}
