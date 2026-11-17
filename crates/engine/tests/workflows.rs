//! Whole journeys through the facade; each test crosses at least two features.

use bookmole::BookQuery;
use bookmole::device::DeviceState;
use bookmole::storage::BookSort;
use bookmole::{Book, NewNoteInput, NoteKind, NoteScope};

mod common;
use common::{engine, place, rewrite_sidecar, seed_book};

#[tokio::test]
async fn a_book_pulled_off_the_device_reflects_on_the_reading_the_import_opened() {
    let (_tmp, engine) = engine().await;
    let device = tempfile::tempdir().unwrap();
    let root = device.path().join("KOBOeReader");
    let sidecar = place(&root, "Gen-Summary.sdr", "Rated.sdr");

    let scan = engine.scan_device(&root).await.unwrap();
    assert_eq!(scan.books.len(), 1);
    assert!(matches!(scan.books[0].state, DeviceState::New { .. }));

    let report = engine.pull_book_from_sidecar(&sidecar).await.unwrap();
    let book = report.stats.book_id;
    assert_eq!(report.stats.inserted, 1);

    let readings = engine.storage().list_readings(book).await.unwrap();
    assert_eq!(readings.len(), 1, "the import opened exactly one reading");
    assert_eq!(readings[0].ko_status.as_deref(), Some("complete"));
    assert_eq!(readings[0].source, "koreader");

    let reflection = engine.open_reflection(book, None).await.unwrap();
    let record = engine
        .storage()
        .get_note(reflection.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        record.reading_id,
        Some(readings[0].id),
        "the reflection must attach to the reading the import opened, not a new one"
    );
    assert_eq!(
        engine.storage().list_readings(book).await.unwrap().len(),
        1,
        "reflecting must not start a second reading"
    );

    let highlights = engine.storage().list_highlights(book).await.unwrap();
    assert_eq!(highlights.len(), 1);
    let review = engine.open_review(book, None).await.unwrap();
    engine.cite(review.id, highlights[0].id).await.unwrap();
    engine.set_rating(review.id, 4.0).await.unwrap();

    assert_eq!(engine.goodreads_rating(review.id).await.unwrap(), Some(4.0));
    let cited = engine.citations_for(review.id).await.unwrap();
    assert_eq!(cited.len(), 1);
    assert_eq!(cited[0].text, "a passage in a finished book");
}

#[tokio::test]
async fn a_device_note_edit_reaches_us_through_the_cache_and_spares_everything_ours() {
    let (_tmp, engine) = engine().await;
    let device = tempfile::tempdir().unwrap();
    let root = device.path().join("KOBOeReader");
    let sidecar = place(&root, "Middlemarch.sdr", "Middlemarch.sdr");
    seed_book(&engine, "Middlemarch").await;

    // Scan before syncing: only a scan populates `sidecar_seen`, so syncing first would make the
    // "unchanged costs nothing" assertion below measure the wrong thing.
    let first = engine.scan_device(&root).await.unwrap();
    assert_eq!(first.parsed, 1);
    engine
        .sync_device(std::slice::from_ref(&sidecar))
        .await
        .unwrap();
    let book = engine
        .storage()
        .list_books(&BookQuery::new(10, BookSort::LastModified))
        .await
        .unwrap()[0]
        .id
        .unwrap();

    let highlights = engine.storage().list_highlights(book).await.unwrap();
    let target = highlights
        .iter()
        .find(|h| {
            h.text
                .starts_with("We are all of us born in moral stupidity")
        })
        .expect("the opening line is in the fixture");
    let before_id = target.id;
    engine
        .storage()
        .set_annotation(before_id, Some("mine, and the device may not touch it"))
        .await
        .unwrap();
    let review = engine.open_review(book, None).await.unwrap();
    engine.cite(review.id, before_id).await.unwrap();
    let anchored = engine
        .create_note(NewNoteInput {
            book_id: Some(book),
            reading_id: None,
            title: Some("On the opening line".into()),
            body: "It sets the register for the whole book.".into(),
            kind: NoteKind::Note,
            page: None,
            location: None,
            highlight_id: Some(before_id),
        })
        .await
        .unwrap();

    // Same identity hash, new payload, so a count-based cache would see nothing move.
    let second = engine.scan_device(&root).await.unwrap();
    assert_eq!(
        second.parsed, 0,
        "unmodified: the pre-filter should skip it"
    );
    rewrite_sidecar(&sidecar, |s| {
        s.replace(
            "Opening line - sets the whole register.",
            "Rewritten on the device, weeks later.",
        )
    });

    let third = engine.scan_device(&root).await.unwrap();
    assert_eq!(third.parsed, 1, "the bytes moved, so it must be re-read");
    assert_eq!(
        third.books[0].state,
        DeviceState::Updated {
            new_highlights: 0,
            refreshed: 1
        },
        "a rewritten note is a refresh, not a new highlight"
    );

    engine
        .sync_device(std::slice::from_ref(&sidecar))
        .await
        .unwrap();

    let after = engine.storage().list_highlights(book).await.unwrap();
    let refreshed = after.iter().find(|h| h.id == before_id).expect(
        "the row must be updated in place — notes.highlight_id and \
         flashcards.highlight_id are foreign keys",
    );
    assert_eq!(
        refreshed.origin_note.as_deref(),
        Some("Rewritten on the device, weeks later."),
        "theirs tracks the device"
    );
    assert_eq!(
        refreshed.annotation.as_deref(),
        Some("mine, and the device may not touch it"),
        "ours survives"
    );
    assert_eq!(refreshed.created_at, target.created_at);
    assert_eq!(refreshed.source, target.source);

    let cited = engine.citations_for(review.id).await.unwrap();
    assert_eq!(cited.len(), 1);
    assert_eq!(cited[0].id, before_id);
    assert_eq!(
        cited[0].origin_note.as_deref(),
        Some("Rewritten on the device, weeks later."),
        "a citation reads through to the live row"
    );

    let note = engine
        .storage()
        .get_note(anchored.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        note.highlight_id,
        Some(before_id),
        "the note anchor must not have been nulled"
    );

    let fourth = engine.scan_device(&root).await.unwrap();
    assert_eq!(fourth.books[0].state, DeviceState::Unchanged);
}

/// Asserted as parse counts, never durations.
#[tokio::test]
async fn a_synced_device_costs_nothing_to_look_at_again() {
    let (_tmp, engine) = engine().await;
    let device = tempfile::tempdir().unwrap();
    let root = device.path().join("KOBOeReader");
    place(&root, "Middlemarch.sdr", "Middlemarch.sdr");
    place(&root, "Multi-Chapter.sdr", "Odyssey.sdr");
    place(&root, "Gen-Summary.sdr", "Rated.sdr");
    seed_book(&engine, "Middlemarch").await;
    seed_book(&engine, "The Odyssey").await;

    let first = engine.scan_device(&root).await.unwrap();
    assert_eq!(first.parsed, 3);
    assert_eq!(first.cached, 0);

    let paths: Vec<_> = first.syncable().map(|b| b.path.clone()).collect();
    engine.sync_device(&paths).await.unwrap();

    for round in 0..3 {
        let again = engine.scan_device(&root).await.unwrap();
        assert_eq!(again.parsed, 0, "round {round}: nothing changed on disk");
        assert_eq!(again.cached, 3, "round {round}");
        assert!(
            again
                .books
                .iter()
                .all(|b| b.state == DeviceState::Unchanged),
            "round {round}: {:?}",
            again.books.iter().map(|b| &b.state).collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn merging_a_duplicate_keeps_both_reflections_and_every_citation() {
    let (_tmp, engine) = engine().await;
    let device = tempfile::tempdir().unwrap();
    let root = device.path().join("KOBOeReader");

    let dst = seed_book(&engine, "Middlemarch").await;
    let dst_reflection = engine.open_reflection(dst, None).await.unwrap();
    let dst_record = engine
        .storage()
        .get_note(dst_reflection.id)
        .await
        .unwrap()
        .unwrap();
    engine
        .update_note_body(&dst_record, "What I thought reading it here.")
        .await
        .unwrap();

    let sidecar = place(&root, "Unmatched.sdr", "Stranger.sdr");
    let pulled = engine.pull_book_from_sidecar(&sidecar).await.unwrap();
    let src = pulled.stats.book_id;
    assert_ne!(src, dst);

    let src_highlights = engine.storage().list_highlights(src).await.unwrap();
    assert!(
        !src_highlights.is_empty(),
        "the fixture carries annotations"
    );
    let src_review = engine.open_review(src, None).await.unwrap();
    engine
        .cite(src_review.id, src_highlights[0].id)
        .await
        .unwrap();
    let src_reflection = engine.open_reflection(src, None).await.unwrap();
    let src_record = engine
        .storage()
        .get_note(src_reflection.id)
        .await
        .unwrap()
        .unwrap();
    engine
        .update_note_body(&src_record, "What I thought reading it there.")
        .await
        .unwrap();

    let report = engine.merge_books(src, dst).await.unwrap();
    assert!(report.src_existed);
    assert_eq!(report.highlights_moved, src_highlights.len());
    assert!(report.readings_moved >= 1);

    let notes = engine.list_notes(NoteScope::Book(dst), None).await.unwrap();
    let reflections: Vec<_> = notes.iter().filter(|n| n.kind == "reflection").collect();
    assert_eq!(
        reflections.len(),
        2,
        "one per reading, which is what anchoring to a reading means"
    );
    let readings: Vec<_> = reflections.iter().map(|n| n.reading_id).collect();
    assert_ne!(readings[0], readings[1], "…and they must not share one");
    assert_eq!(
        engine.note_body(&src_record).unwrap(),
        "What I thought reading it there.",
        "the merged-in reflection keeps its text"
    );
    assert_eq!(
        engine.note_body(&dst_record).unwrap(),
        "What I thought reading it here."
    );

    let cited = engine.citations_for(src_review.id).await.unwrap();
    assert_eq!(
        cited.len(),
        1,
        "a citation must survive its book being merged"
    );
    assert_eq!(cited[0].book_id, dst);

    let again = engine.merge_books(src, dst).await.unwrap();
    assert!(!again.src_existed);
    assert_eq!(again.highlights_moved, 0);
}

#[tokio::test]
async fn a_reflection_links_to_another_and_the_graph_follows_the_body() {
    let (_tmp, engine) = engine().await;
    let middlemarch = seed_book(&engine, "Middlemarch").await;
    let other = engine
        .save_book(&Book {
            title: Some("Silas Marner".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();

    let a = engine.open_reflection(middlemarch, None).await.unwrap();
    let b = engine.open_reflection(other, None).await.unwrap();
    let a_rec = engine.storage().get_note(a.id).await.unwrap().unwrap();

    engine
        .update_note_body(
            &a_rec,
            &format!(
                "Same preoccupations as [[{}]], twenty years earlier.",
                b.title
            ),
        )
        .await
        .unwrap();

    let links = engine.storage().note_links(a.id).await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].0, b.title);
    assert_eq!(
        links[0].1,
        Some(b.id),
        "the target exists, so the edge must resolve rather than dangle"
    );

    let hits = engine
        .search_marks(
            "preoccupations",
            Some(bookmole::SearchSource::Note),
            None,
            10,
        )
        .await
        .unwrap();
    assert!(
        hits.iter()
            .any(|h| h.as_note().is_some_and(|n| n.id == a.id)),
        "a rewritten body has to reach the search index"
    );

    engine
        .update_note_body(&a_rec, "On reflection, they are not much alike.")
        .await
        .unwrap();
    assert!(
        engine.storage().note_links(a.id).await.unwrap().is_empty(),
        "a deleted wikilink must leave the graph too"
    );
    assert!(
        engine
            .search_marks(
                "preoccupations",
                Some(bookmole::SearchSource::Note),
                None,
                10
            )
            .await
            .unwrap()
            .is_empty(),
        "and the old body must leave the index"
    );
}

#[tokio::test]
async fn an_edit_made_outside_the_app_reaches_the_index_and_the_graph() {
    let (tmp, engine) = engine().await;
    let middlemarch = seed_book(&engine, "Middlemarch").await;
    let other = seed_book(&engine, "Silas Marner").await;

    let a = engine.open_reflection(middlemarch, None).await.unwrap();
    let b = engine.open_reflection(other, None).await.unwrap();
    let a_rec = engine.storage().get_note(a.id).await.unwrap().unwrap();

    let file = tmp.path().join("vault").join(&a_rec.file_path);
    let existing = std::fs::read_to_string(&file).unwrap();
    let (frontmatter, _) = existing.split_at(existing.rfind("---\n").unwrap() + 4);
    std::fs::write(
        &file,
        format!(
            "{frontmatter}\nEdited elsewhere. See [[{}]] — antechamber.\n",
            b.title
        ),
    )
    .unwrap();

    assert!(
        engine
            .search_marks("antechamber", Some(bookmole::SearchSource::Note), None, 10)
            .await
            .unwrap()
            .is_empty(),
        "an outside edit is invisible until we are told to re-read it"
    );

    engine.refresh_note_from_disk(&a_rec).await.unwrap();

    let hits = engine
        .search_marks("antechamber", Some(bookmole::SearchSource::Note), None, 10)
        .await
        .unwrap();
    assert!(
        hits.iter()
            .any(|h| h.as_note().is_some_and(|n| n.id == a.id)),
        "the index followed"
    );

    let links = engine.storage().note_links(a.id).await.unwrap();
    assert_eq!(links.len(), 1, "and so did the graph");
    assert_eq!(links[0].1, Some(b.id));
}

#[tokio::test]
async fn a_link_the_user_made_by_hand_survives_every_later_scan() {
    let (_tmp, engine) = engine().await;
    let device = tempfile::tempdir().unwrap();
    let root = device.path().join("KOBOeReader");
    let sidecar = place(&root, "Unmatched.sdr", "Stranger.sdr");

    let book = seed_book(&engine, "Middlemarch").await;
    let scan = engine.scan_device(&root).await.unwrap();
    assert!(
        matches!(scan.books[0].state, DeviceState::New { .. }),
        "precondition: the matcher does not find it"
    );
    assert_eq!(scan.books[0].book_id, None);

    engine.link_sidecar(&sidecar, book).await.unwrap();

    for round in 0..3 {
        let after = engine.scan_device(&root).await.unwrap();
        assert_eq!(
            after.books[0].book_id,
            Some(book),
            "round {round}: the recorded decision must be used"
        );
        assert_eq!(
            after.books[0].matched_by,
            Some(bookmole::MatchMethod::Md5),
            "round {round}: matched by the recorded link, not re-guessed by title"
        );
        assert_ne!(
            after.books[0].state,
            DeviceState::New {
                candidates: Vec::new()
            },
            "round {round}: a linked sidecar is never New again"
        );
    }
}

#[tokio::test]
async fn a_file_imported_later_lands_on_the_book_the_device_already_created() {
    let (tmp, engine) = engine().await;
    let device = tempfile::tempdir().unwrap();
    let root = device.path().join("KOBOeReader");
    let sidecar = place(&root, "Middlemarch.sdr", "Middlemarch.sdr");

    let pulled = engine.pull_book_from_sidecar(&sidecar).await.unwrap();
    let book = pulled.stats.book_id;
    assert!(pulled.stats.inserted > 0, "the pull brought highlights in");

    // No ISBN, as a sidecar-created book has none, so the ISBN rung cannot be what matches.
    let file = tmp.path().join("middlemarch-min-jin-lee.epub");
    // The sidecar's own author: the matcher refuses to fold two different names for one title.
    common::write_isbnless_epub_by(&file, "Middlemarch", "George Eliot");
    let report = engine
        .import_file(&file, bookmole::FileImportOptions::default())
        .await
        .unwrap();

    assert_eq!(report.book_id, Some(book), "one book, not two");
    assert_eq!(report.matched_by, Some(bookmole::FileMatch::Title));
    assert_eq!(
        engine
            .storage()
            .list_books(&BookQuery::new(100, BookSort::Title))
            .await
            .unwrap()
            .len(),
        1
    );
    // Pointed to, not copied. The bytes do not match the reader place's `partial_md5`, so this is
    // a second copy with a `path` place; a matching md5 would be the same copy.
    assert!(engine.book_files(book).await.unwrap().is_empty());
    let copies = engine.copies_for_book(book).await.unwrap();
    assert_eq!(copies.len(), 2);
    assert!(copies.iter().any(|c| {
        c.places
            .iter()
            .any(|p| p.kind == bookmole::copies::PlaceKind::Path)
    }));
    assert!(
        !engine
            .storage()
            .list_highlights(book)
            .await
            .unwrap()
            .is_empty(),
        "and the highlights are still the same book's"
    );
}
