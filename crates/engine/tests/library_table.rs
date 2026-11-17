//! The library as a table: every book, the latest read's state and how many
//! days it took, the counts per filter, and the sort positions a frontend
//! sorts by (so it never re-collates a title itself).

mod common;

use bookmole::{Book, Engine, LearnedFrom, NewCopy, ReadingState, Tenure};

fn at(day: &str) -> i64 {
    bookmole::history::midnight(bookmole::history::parse_day(day).unwrap()) + 12 * 3600
}

/// A book you have: the table lists the library, and a book is in it by a copy.
async fn book(engine: &Engine, title: &str, author: &str, pages: Option<i64>) -> i64 {
    let id = engine
        .save_book(&Book {
            title: Some(title.into()),
            authors: vec![author.into()],
            page_count: pages,
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    engine
        .add_copy(&NewCopy::new(id, None, Tenure::Own, LearnedFrom::User))
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn every_book_is_a_row_and_the_counts_partition_the_shelf() {
    let (_tmp, engine) = common::engine().await;
    let s = engine.storage();
    let yellow = book(&engine, "The Yellow Hours", "Fen Halloran", Some(179)).await;
    let bell = book(&engine, "Hollow Bell", "Mariam Halloran", Some(318)).await;
    let orchard = book(&engine, "The Pale Orchard", "Nadia Grigorescu", Some(356)).await;
    let arith = book(&engine, "A Brief Arithmetic", "Yuki Dziedzic", Some(232)).await;
    let never = book(&engine, "Salt Machine", "Owen Dziedzic", None).await;

    // A reread: the latest read is the second one.
    s.record_reading(
        yellow,
        Some(at("2025-01-01")),
        Some(at("2025-01-20")),
        "finished",
        "manual",
    )
    .await
    .unwrap();
    s.record_reading(
        yellow,
        Some(at("2026-10-09")),
        Some(at("2026-10-28")),
        "finished",
        "manual",
    )
    .await
    .unwrap();
    s.record_reading(bell, Some(at("2026-10-01")), None, "reading", "manual")
        .await
        .unwrap();
    s.record_reading(orchard, Some(at("2026-08-20")), None, "reading", "manual")
        .await
        .unwrap();
    s.abandon_reading(orchard).await.unwrap();
    s.record_reading(
        arith,
        Some(at("2026-07-30")),
        Some(at("2026-08-28")),
        "finished",
        "manual",
    )
    .await
    .unwrap();
    s.insert_highlight(arith, &common::highlight("one", "2026-08-01 09:00:00"))
        .await
        .unwrap();

    let table = engine.library_table().await.unwrap();
    assert_eq!(table.rows.len(), 5, "the never-started book is a row too");
    let c = table.counts;
    assert_eq!(
        (c.all, c.reading, c.read, c.put_down, c.other, c.no_reading),
        (5, 1, 2, 1, 0, 1)
    );
    assert_eq!(
        c.reading + c.read + c.put_down + c.other + c.no_reading,
        c.all
    );
    assert_eq!(table.total_pages, 179 + 318 + 356 + 232);

    let row = |id: i64| table.rows.iter().find(|r| r.book.id == Some(id)).unwrap();
    let y = row(yellow);
    let latest = y.latest.as_ref().unwrap();
    assert_eq!(latest.state, ReadingState::Finished);
    assert_eq!(latest.days, Some(19));
    assert_eq!(
        (latest.count.number, latest.count.of),
        (2, 2),
        "the reread, not the first read"
    );
    assert!(row(never).latest.is_none());
    assert_eq!(row(arith).passages, 1);
    let o = row(orchard).latest.as_ref().unwrap();
    assert_eq!(o.state, ReadingState::Abandoned);
    assert!(
        o.put_down_at.is_some(),
        "abandon_reading stamps the instant (0025)"
    );

    // Title order drops the article: (A) Brief Arithmetic, Hollow Bell, (The)
    // Pale Orchard, Salt Machine, (The) Yellow Hours.
    let by_title: Vec<i64> = table.rows.iter().map(|r| r.book.id.unwrap()).collect();
    assert_eq!(by_title, vec![arith, bell, orchard, never, yellow]);
    assert!(
        table
            .rows
            .windows(2)
            .all(|w| w[0].title_order < w[1].title_order)
    );

    // Author order is the engine's surname order: Dziedzic (Owen), Dziedzic
    // (Yuki), Grigorescu, Halloran (Fen), Halloran (Mariam).
    let mut by_author: Vec<(i64, i64)> = table
        .rows
        .iter()
        .map(|r| (r.author_order, r.book.id.unwrap()))
        .collect();
    by_author.sort();
    assert_eq!(
        by_author.into_iter().map(|(_, id)| id).collect::<Vec<_>>(),
        vec![never, arith, orchard, yellow, bell]
    );
}
