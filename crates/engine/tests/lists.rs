mod common;

use bookmole::{
    Book, Engine, EngineError, EntryShow, EntrySort, LearnedFrom, ListSort, NewCopy, Tenure,
};
use common::{engine, seed_book};

async fn dated(engine: &Engine, title: &str, year: Option<i64>) -> i64 {
    engine
        .save_book(&Book {
            title: Some(title.into()),
            publish_year: year,
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap()
}

async fn order(engine: &Engine, list: i64) -> Vec<i64> {
    let entries = engine
        .list_entries(list, EntryShow::All, EntrySort::Yours)
        .await
        .unwrap();
    let positions: Vec<i64> = entries.iter().map(|e| e.position).collect();
    assert_eq!(
        positions,
        (0..entries.len() as i64).collect::<Vec<_>>(),
        "positions are exactly 0..n"
    );
    entries.iter().map(|e| e.book.id.unwrap()).collect()
}

#[tokio::test]
async fn add_move_remove_renumber() {
    let (_tmp, e) = engine().await;
    let [a, b, c, d] = [
        seed_book(&e, "A").await,
        seed_book(&e, "B").await,
        seed_book(&e, "C").await,
        seed_book(&e, "D").await,
    ];
    let list = e.create_list("To read", None).await.unwrap().id;
    for id in [a, b, c, d] {
        e.add_to_list(list, id, None).await.unwrap();
    }
    assert_eq!(order(&e, list).await, [a, b, c, d]);

    e.move_in_list(list, d, 0).await.unwrap();
    assert_eq!(order(&e, list).await, [d, a, b, c]);
    e.move_in_list(list, d, 2).await.unwrap();
    assert_eq!(order(&e, list).await, [a, b, d, c]);
    e.move_in_list(list, a, 99).await.unwrap();
    assert_eq!(order(&e, list).await, [b, d, c, a]);

    e.remove_from_list(list, d).await.unwrap();
    assert_eq!(order(&e, list).await, [b, c, a]);
    e.remove_from_list(list, d).await.unwrap();
    assert_eq!(order(&e, list).await, [b, c, a]);

    let by_title = e
        .list_entries(list, EntryShow::All, EntrySort::Title)
        .await
        .unwrap();
    assert_eq!(
        by_title
            .iter()
            .map(|x| x.book.id.unwrap())
            .collect::<Vec<_>>(),
        [a, b, c]
    );
    assert_eq!(order(&e, list).await, [b, c, a]);

    assert!(matches!(
        e.move_in_list(list, d, 0).await,
        Err(EngineError::NotFound(_))
    ));
}

#[tokio::test]
async fn adding_twice_is_a_no_op() {
    let (_tmp, e) = engine().await;
    let (a, b) = (seed_book(&e, "A").await, seed_book(&e, "B").await);
    let list = e.create_list("L", None).await.unwrap().id;
    let first = e.add_to_list(list, a, Some("first note")).await.unwrap();
    e.add_to_list(list, b, None).await.unwrap();
    let again = e.add_to_list(list, a, Some("other note")).await.unwrap();
    assert_eq!(again.position, first.position);
    assert_eq!(again.note.as_deref(), Some("first note"));
    assert_eq!(again.added_at, first.added_at);
    assert_eq!(order(&e, list).await, [a, b]);
}

#[tokio::test]
async fn errors_are_typed() {
    let (_tmp, e) = engine().await;
    let a = seed_book(&e, "A").await;
    assert!(matches!(
        e.create_list("   ", None).await,
        Err(EngineError::InvalidInput(_))
    ));
    let list = e.create_list("L", None).await.unwrap().id;
    assert!(matches!(
        e.update_list(list, "", None).await,
        Err(EngineError::InvalidInput(_))
    ));
    assert!(matches!(
        e.add_to_list(999, a, None).await,
        Err(EngineError::NotFound(_))
    ));
    assert!(matches!(
        e.add_to_list(list, 999, None).await,
        Err(EngineError::NotFound(_))
    ));
    assert!(matches!(
        e.get_list(999).await,
        Err(EngineError::NotFound(_))
    ));
    assert!(matches!(
        e.delete_list(999).await,
        Err(EngineError::NotFound(_))
    ));
    e.create_list("L", None).await.unwrap();
}

#[tokio::test]
async fn empty_strings_are_stored_as_null() {
    let (_tmp, e) = engine().await;
    let a = seed_book(&e, "A").await;
    let list = e.create_list("L", Some("  \n ")).await.unwrap();
    assert_eq!(list.description, None);
    let list = e.update_list(list.id, "L", Some("")).await.unwrap();
    assert_eq!(list.description, None);
    let list = e.update_list(list.id, "L", Some("words")).await.unwrap();
    assert_eq!(list.description.as_deref(), Some("words"));

    let entry = e.add_to_list(list.id, a, Some("  ")).await.unwrap();
    assert_eq!(entry.note, None);
    e.set_list_note(list.id, a, Some("a note")).await.unwrap();
    let got = e
        .list_entries(list.id, EntryShow::All, EntrySort::Yours)
        .await
        .unwrap();
    assert_eq!(got[0].note.as_deref(), Some("a note"));
    e.set_list_note(list.id, a, Some("")).await.unwrap();
    let got = e
        .list_entries(list.id, EntryShow::All, EntrySort::Yours)
        .await
        .unwrap();
    assert_eq!(got[0].note, None);
}

#[tokio::test]
async fn merging_books_that_share_a_list() {
    let (_tmp, e) = engine().await;
    let [a, b, c, d] = [
        seed_book(&e, "A").await,
        seed_book(&e, "B").await,
        seed_book(&e, "C").await,
        seed_book(&e, "D").await,
    ];
    let shared = e.create_list("shared", None).await.unwrap().id;
    let only_src = e.create_list("only src", None).await.unwrap().id;
    e.add_to_list(shared, a, None).await.unwrap();
    e.add_to_list(shared, b, Some("from src")).await.unwrap();
    e.add_to_list(shared, c, None).await.unwrap();
    e.add_to_list(shared, d, Some("from dst")).await.unwrap();
    e.add_to_list(only_src, b, None).await.unwrap();

    e.merge_books(b, d).await.unwrap();

    assert_eq!(order(&e, shared).await, [a, d, c]);
    let got = e
        .list_entries(shared, EntryShow::All, EntrySort::Yours)
        .await
        .unwrap();
    assert_eq!(got[1].note.as_deref(), Some("from src\n\nfrom dst"));
    assert_eq!(order(&e, only_src).await, [d]);
    assert_eq!(e.lists_for_book(d).await.unwrap().len(), 2);

    let [x, y] = [seed_book(&e, "X").await, seed_book(&e, "Y").await];
    let l = e.create_list("l", None).await.unwrap().id;
    e.add_to_list(l, x, Some("x")).await.unwrap();
    e.add_to_list(l, a, None).await.unwrap();
    e.add_to_list(l, y, Some("y")).await.unwrap();
    e.merge_books(y, x).await.unwrap();
    assert_eq!(order(&e, l).await, [x, a]);
    let got = e
        .list_entries(l, EntryShow::All, EntrySort::Yours)
        .await
        .unwrap();
    assert_eq!(got[0].note.as_deref(), Some("x\n\ny"));
}

#[tokio::test]
async fn deleting_a_book_cascades_and_renumbers() {
    let (_tmp, e) = engine().await;
    let [a, b, c] = [
        seed_book(&e, "A").await,
        seed_book(&e, "B").await,
        seed_book(&e, "C").await,
    ];
    let l1 = e.create_list("one", None).await.unwrap().id;
    let l2 = e.create_list("two", None).await.unwrap().id;
    for id in [a, b, c] {
        e.add_to_list(l1, id, None).await.unwrap();
    }
    e.add_to_list(l2, c, None).await.unwrap();
    e.add_to_list(l2, b, None).await.unwrap();

    e.delete_book(a).await.unwrap();
    assert_eq!(order(&e, l1).await, [b, c]);
    e.delete_book(b).await.unwrap();
    assert_eq!(order(&e, l1).await, [c]);
    assert_eq!(order(&e, l2).await, [c]);
    assert_eq!(e.get_list(l1).await.unwrap().id, l1);
}

#[tokio::test]
async fn summaries_membership_and_stamps() {
    let (_tmp, e) = engine().await;
    let [a, b, c] = [
        seed_book(&e, "A").await,
        seed_book(&e, "B").await,
        seed_book(&e, "C").await,
    ];
    for id in [a, b] {
        e.add_copy(&NewCopy::new(id, None, Tenure::Own, LearnedFrom::User))
            .await
            .unwrap();
    }
    let l = e.create_list("Zebra", None).await.unwrap().id;
    let m = e.create_list("apple", None).await.unwrap().id;
    for id in [a, b, c] {
        e.add_to_list(l, id, None).await.unwrap();
    }
    e.add_to_list(m, b, Some("n")).await.unwrap();

    let by_name = e.list_lists(ListSort::Name, 2).await.unwrap();
    assert_eq!(by_name[0].list.id, m);
    assert_eq!(by_name[1].book_count, 3);
    assert_eq!(by_name[1].not_in_library, 1);
    let faces: Vec<i64> = by_name[1].preview.iter().map(|b| b.id.unwrap()).collect();
    assert_eq!(faces, [a, b], "first two by position");

    let mem = e.lists_for_book(b).await.unwrap();
    assert_eq!(mem.len(), 2);
    let on_zebra = mem.iter().find(|x| x.list_id == l).unwrap();
    assert_eq!((on_zebra.position, on_zebra.list_size), (2, 3));
    assert_eq!(
        mem.iter().find(|x| x.list_id == m).unwrap().note.as_deref(),
        Some("n")
    );

    assert!(matches!(
        e.lists_for_book(999).await,
        Err(EngineError::NotFound(_))
    ));
    assert_eq!(
        e.list_entries(l, EntryShow::NotInLibrary, EntrySort::Yours)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        e.list_entries(l, EntryShow::InLibrary, EntrySort::Yours)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn keep_and_dismiss_shelf_suggestions() {
    let (_tmp, e) = engine().await;
    let [a, b, c] = [
        seed_book(&e, "Zeta").await,
        seed_book(&e, "Alpha").await,
        seed_book(&e, "Mid").await,
    ];
    let tag = |t: &str| (t.to_string(), t.to_string());
    let s = e.storage();
    for id in [a, b, c] {
        s.add_book_tags(id, "goodreads", &[tag("to-read")])
            .await
            .unwrap();
    }
    s.add_book_tags(b, "goodreads", &[tag("favourites")])
        .await
        .unwrap();
    s.add_book_tags(a, "goodreads", &[tag("skipped")])
        .await
        .unwrap();
    s.add_book_tags(a, "calibre", &[tag("fiction")])
        .await
        .unwrap();

    let got = e.shelf_suggestions(2).await.unwrap();
    let tags: Vec<_> = got.iter().map(|g| g.tag.as_str()).collect();
    assert_eq!(tags, ["to-read", "favourites", "skipped"]);
    assert_eq!(got[0].book_count, 3);
    assert_eq!(got[0].preview.len(), 2);
    assert_eq!(got[0].source, "goodreads");

    let kept = e.keep_shelf("goodreads", "to-read").await.unwrap();
    assert_eq!(kept.source, "goodreads");
    assert_eq!(kept.source_ref.as_deref(), Some("to-read"));
    assert_eq!(order(&e, kept.id).await, [a, b, c]);
    assert_eq!(
        e.keep_shelf("goodreads", "to-read").await.unwrap().id,
        kept.id
    );
    s.add_book_tags(seed_book(&e, "Late").await, "goodreads", &[tag("to-read")])
        .await
        .unwrap();
    assert_eq!(order(&e, kept.id).await, [a, b, c]);

    e.dismiss_shelf("goodreads", "skipped").await.unwrap();
    e.dismiss_shelf("goodreads", "skipped").await.unwrap();
    let tags: Vec<_> = e
        .shelf_suggestions(0)
        .await
        .unwrap()
        .into_iter()
        .map(|g| g.tag)
        .collect();
    assert_eq!(tags, ["favourites"]);

    assert!(matches!(
        e.keep_shelf("goodreads", "no-such-shelf").await,
        Err(EngineError::NotFound(_))
    ));
    assert!(matches!(
        e.keep_shelf("calibre", "fiction").await,
        Err(EngineError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn timeline_is_ready_at_three_dated_books_not_two() {
    let (_tmp, e) = engine().await;
    let l = e.create_list("t", None).await.unwrap().id;
    let u = dated(&e, "undated", None).await;
    let y1 = dated(&e, "y1", Some(1990)).await;
    let y2 = dated(&e, "y2", Some(1970)).await;
    for id in [u, y1, y2] {
        e.add_to_list(l, id, None).await.unwrap();
    }
    let t = e.list_timeline(l).await.unwrap();
    assert!(!t.ready, "two dated books is not enough");
    assert_eq!((t.dated.len(), t.undated.len()), (2, 1));

    let y3 = dated(&e, "y3", Some(2000)).await;
    e.add_to_list(l, y3, None).await.unwrap();
    let t = e.list_timeline(l).await.unwrap();
    assert!(t.ready);
    let ids =
        |v: &[bookmole::ListEntry]| -> Vec<i64> { v.iter().map(|x| x.book.id.unwrap()).collect() };
    assert_eq!(ids(&t.dated), [y1, y2, y3]);
    assert_eq!(ids(&t.undated), [u]);

    let pubd = e
        .list_entries(l, EntryShow::All, EntrySort::Published)
        .await
        .unwrap();
    assert_eq!(ids_of(&pubd), [y2, y1, y3, u]);
    assert_eq!(order(&e, l).await, [u, y1, y2, y3]);
}

fn ids_of(v: &[bookmole::ListEntry]) -> Vec<i64> {
    v.iter().map(|x| x.book.id.unwrap()).collect()
}

#[tokio::test]
async fn the_year_prefers_first_publication_and_flags_an_edition() {
    let (_tmp, e) = engine().await;
    let l = e.create_list("t", None).await.unwrap().id;
    let both = e
        .save_book(&Book {
            title: Some("both".into()),
            publish_year: Some(2011),
            first_published: Some(1949),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    let edition_only = dated(&e, "edition only", Some(1980)).await;
    let neither = dated(&e, "neither", None).await;
    for id in [both, edition_only, neither] {
        e.add_to_list(l, id, None).await.unwrap();
    }

    let by_pub = e
        .list_entries(l, EntryShow::All, EntrySort::Published)
        .await
        .unwrap();
    assert_eq!(ids_of(&by_pub), [both, edition_only, neither]);
    let got: Vec<_> = by_pub.iter().map(|x| (x.year, x.year_is_edition)).collect();
    assert_eq!(
        got,
        [(Some(1949), false), (Some(1980), true), (None, false)]
    );
    assert_eq!(by_pub[0].book.first_published, Some(1949));
    assert_eq!(by_pub[0].book.publish_year, Some(2011));
}

#[tokio::test]
async fn deleting_a_list_keeps_its_books() {
    let (_tmp, e) = engine().await;
    let a = seed_book(&e, "A").await;
    let l = e.create_list("L", None).await.unwrap().id;
    e.add_to_list(l, a, None).await.unwrap();
    e.delete_list(l).await.unwrap();
    assert!(e.get_book(a).await.unwrap().is_some());
    assert!(e.lists_for_book(a).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_candidate_matching_a_book_we_have_is_reused() {
    use bookmole::BookQuery;
    let (_tmp, e) = engine().await;
    let have = e
        .save_book(&Book {
            title: Some("Middlemarch".into()),
            authors: vec!["George Eliot".into()],
            isbn_13: Some("9781900000178".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    e.add_copy(&NewCopy::new(have, None, Tenure::Own, LearnedFrom::User))
        .await
        .unwrap();
    let l = e.create_list("L", None).await.unwrap().id;

    let by_isbn = e
        .add_candidate_to_list(
            l,
            &Book {
                title: Some("Middlemarch (a different edition title)".into()),
                isbn_13: Some("9781900000178".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(by_isbn.book.id, Some(have));
    assert!(by_isbn.in_library);

    let by_title = e
        .add_candidate_to_list(
            l,
            &Book {
                title: Some("Middlemarch".into()),
                authors: vec!["George Eliot".into()],
                isbn_13: Some("9781900000185".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(by_title.book.id, Some(have));
    assert_eq!(e.list_books(&BookQuery::default()).await.unwrap().len(), 1);
    assert_eq!(order(&e, l).await, [have]);
}

#[tokio::test]
async fn a_new_candidate_is_a_book_without_a_copy_and_twice_is_one() {
    use bookmole::{BookFilter, BookQuery, LibraryScope};
    let (_tmp, e) = engine().await;
    let l = e.create_list("L", None).await.unwrap().id;
    let cand = Book {
        title: Some("The Dispossessed".into()),
        authors: vec!["Ursula K. Le Guin".into()],
        isbn_13: Some("9780061054884".into()),
        ..Default::default()
    };
    let first = e.add_candidate_to_list(l, &cand, Some("n")).await.unwrap();
    let id = first.book.id.unwrap();
    assert!(!first.in_library);
    assert!(e.copies_for_book(id).await.unwrap().is_empty());
    assert!(e.get_book(id).await.unwrap().is_some());

    let again = e.add_candidate_to_list(l, &cand, None).await.unwrap();
    assert_eq!(again.book.id, Some(id));
    assert_eq!(again.note.as_deref(), Some("n"));
    assert_eq!(order(&e, l).await, [id]);

    let q = |scope| {
        BookQuery::default().with_filter(BookFilter {
            library: scope,
            ..Default::default()
        })
    };
    assert!(
        e.list_books(&q(LibraryScope::InLibrary))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(e.list_books(&q(LibraryScope::All)).await.unwrap().len(), 1);
    let summary = &e.list_lists(ListSort::Name, 0).await.unwrap()[0];
    assert_eq!((summary.book_count, summary.not_in_library), (1, 1));

    assert!(matches!(
        e.add_candidate_to_list(9999, &cand, None).await,
        Err(EngineError::NotFound(_))
    ));
}
