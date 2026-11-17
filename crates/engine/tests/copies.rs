//! Copies: the in-library rule, `touched`, places, merging and the library scope on the book list.

mod common;
use bookmole::{
    BookFilter, BookQuery, Confidence, Copy, CopyForm, Engine, LearnedFrom, LibraryScope, NewCopy,
    NewPlace, NewReadingEvent, PlaceAttach, PlaceKind, Tenure,
};
use bookmole::{NewNoteInput, NoteKind};
use common::{engine, highlight, seed_book};

async fn copy_of(engine: &Engine, book_id: i64, have_now: Option<bool>, tenure: Tenure) -> Copy {
    let mut new = NewCopy::new(book_id, Some(CopyForm::Print), tenure, LearnedFrom::User);
    new.have_now = have_now;
    engine.add_copy(&new).await.unwrap()
}

#[derive(Clone, Copy, Debug)]
enum Touch {
    Reading,
    Event,
    Highlight,
    Note,
    Flashcard,
}

async fn touch(engine: &Engine, book_id: i64, how: Touch) {
    let s = engine.storage();
    match how {
        Touch::Reading => {
            s.update_progress(book_id, Some(10), None).await.unwrap();
        }
        Touch::Event => {
            s.record_reading_events(&[NewReadingEvent {
                book_id,
                reading_id: None,
                day: "2026-01-02".into(),
                minutes: Some(5),
                pages: None,
                source: "manual".into(),
                confidence: Confidence::Measured,
            }])
            .await
            .unwrap();
        }
        Touch::Highlight => {
            s.insert_highlight(book_id, &highlight("a line", "2026-01-02 10:00:00"))
                .await
                .unwrap();
        }
        Touch::Note => {
            engine
                .create_note(NewNoteInput {
                    book_id: Some(book_id),
                    kind: NoteKind::Note,
                    title: Some("thought".into()),
                    body: "x".into(),
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        Touch::Flashcard => {
            engine
                .create_flashcard(book_id, None, "mot", None)
                .await
                .unwrap();
        }
    }
}

const ALL_TOUCHES: [Touch; 5] = [
    Touch::Reading,
    Touch::Event,
    Touch::Highlight,
    Touch::Note,
    Touch::Flashcard,
];

#[tokio::test]
async fn the_in_library_truth_table() {
    let (_tmp, engine) = engine().await;
    let cases = [
        (Some(true), false, true),
        (Some(true), true, true),
        (None, false, true),
        (None, true, true),
        (Some(false), false, false),
        (Some(false), true, true),
    ];
    for (i, (have, touched, expect)) in cases.into_iter().enumerate() {
        let b = seed_book(&engine, &format!("Book {i}")).await;
        copy_of(&engine, b, have, Tenure::Borrowed).await;
        if touched {
            touch(&engine, b, Touch::Highlight).await;
        }
        assert_eq!(engine.touched(b).await.unwrap(), touched, "{have:?}");
        assert_eq!(
            engine.in_library(b).await.unwrap(),
            expect,
            "have_now {have:?}, touched {touched}"
        );
    }

    let bare = seed_book(&engine, "Bare").await;
    assert!(!engine.in_library(bare).await.unwrap());
    touch(&engine, bare, Touch::Note).await;
    assert!(!engine.in_library(bare).await.unwrap());
}

#[tokio::test]
async fn one_live_copy_is_enough_and_two_print_copies_are_two() {
    let (_tmp, engine) = engine().await;
    let b = seed_book(&engine, "Signed").await;
    let mut a = NewCopy::new(b, Some(CopyForm::Print), Tenure::Own, LearnedFrom::User);
    a.note = Some("paperback".into());
    let a = engine.add_copy(&a).await.unwrap();
    let mut h = NewCopy::new(b, Some(CopyForm::Print), Tenure::Own, LearnedFrom::User);
    h.note = Some("signed hardcover".into());
    h.have_now = Some(true);
    let h = engine.add_copy(&h).await.unwrap();
    assert_eq!(engine.copies_for_book(b).await.unwrap().len(), 2);

    let mut a = a;
    a.have_now = Some(false);
    assert!(engine.update_copy(&a).await.unwrap());
    assert!(engine.in_library(b).await.unwrap(), "the hardcover is held");
    let mut h = h;
    h.have_now = Some(false);
    engine.update_copy(&h).await.unwrap();
    assert!(!engine.in_library(b).await.unwrap());
}

#[tokio::test]
async fn touched_per_source() {
    let (_tmp, engine) = engine().await;
    for how in ALL_TOUCHES {
        let b = seed_book(&engine, &format!("{how:?}")).await;
        assert!(!engine.touched(b).await.unwrap(), "{how:?} before");
        copy_of(&engine, b, Some(true), Tenure::Own).await;
        assert!(!engine.touched(b).await.unwrap(), "a copy is not a touch");
        touch(&engine, b, how).await;
        assert!(engine.touched(b).await.unwrap(), "{how:?}");
    }
}

#[tokio::test]
async fn the_engine_never_ends_a_copy_by_itself() {
    let (_tmp, engine) = engine().await;
    let b = seed_book(&engine, "Libby").await;
    let mut new = NewCopy::new(b, Some(CopyForm::App), Tenure::Borrowed, LearnedFrom::User);
    new.app = Some("Libby".into());
    new.have_now = Some(true);
    new.due_at = Some(1);
    new.lent_to = Some("  ".into());
    let c = engine.add_copy(&new).await.unwrap();
    assert_eq!(c.lent_to, None);
    engine
        .attach_place(c.id, &NewPlace::new(PlaceKind::Path, "/tmp/x.epub"))
        .await
        .unwrap();
    engine
        .mark_place_missing(PlaceKind::Path, "/tmp/x.epub", 5)
        .await
        .unwrap();
    let c = engine.get_copy(c.id).await.unwrap().unwrap();
    assert_eq!(c.have_now, Some(true), "missing and overdue are facts only");
    assert_eq!(c.places[0].missing_since, Some(5));
    assert!(engine.in_library(b).await.unwrap());

    let o = seed_book(&engine, "Lent").await;
    let mut lent = NewCopy::new(o, Some(CopyForm::Print), Tenure::Own, LearnedFrom::User);
    lent.have_now = Some(true);
    lent.lent_to = Some("Sam".into());
    engine.add_copy(&lent).await.unwrap();
    assert!(engine.in_library(o).await.unwrap());
}

#[tokio::test]
async fn read_without_copy_needs_a_finished_reading_and_no_copy() {
    let (_tmp, engine) = engine().await;
    let b = seed_book(&engine, "Finished").await;
    assert!(
        !engine.read_without_copy(b).await.unwrap(),
        "no reading yet"
    );
    engine
        .storage()
        .update_progress(b, Some(5), None)
        .await
        .unwrap();
    assert!(!engine.read_without_copy(b).await.unwrap(), "still open");
    assert!(engine.storage().finish_reading(b).await.unwrap());
    assert!(engine.read_without_copy(b).await.unwrap());
    let s = engine.book_standing(b).await.unwrap();
    assert!(s.read_without_copy && s.touched && !s.in_library);

    copy_of(&engine, b, None, Tenure::Unrecorded).await;
    assert!(!engine.read_without_copy(b).await.unwrap());
    assert!(engine.in_library(b).await.unwrap());
}

#[tokio::test]
async fn places_refresh_go_missing_and_come_back() {
    let (_tmp, engine) = engine().await;
    let b = seed_book(&engine, "Annotated PDF").await;
    let c = copy_of(&engine, b, Some(true), Tenure::Own).await;
    let mut p = NewPlace::new(PlaceKind::Path, "/Users/me/a.pdf");
    p.last_hash = Some("aaa".into());
    p.last_mtime = Some(100);
    assert_eq!(
        engine.attach_place(c.id, &p).await.unwrap(),
        PlaceAttach::Added
    );
    assert_eq!(
        engine.attach_place(c.id, &p).await.unwrap(),
        PlaceAttach::Present
    );

    assert!(
        engine
            .refresh_place(PlaceKind::Path, "/Users/me/a.pdf", Some("bbb"), Some(200))
            .await
            .unwrap()
    );
    let found = engine
        .copy_for_place(PlaceKind::Path, "/Users/me/a.pdf")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, c.id);
    assert_eq!(found.places[0].last_hash.as_deref(), Some("bbb"));
    assert_eq!(found.places[0].last_mtime, Some(200));
    assert!(found.places[0].last_seen_at.is_some());

    assert!(
        engine
            .mark_place_missing(PlaceKind::Path, "/Users/me/a.pdf", 10)
            .await
            .unwrap()
    );
    assert!(
        !engine
            .mark_place_missing(PlaceKind::Path, "/Users/me/a.pdf", 99)
            .await
            .unwrap()
    );
    let c2 = engine.get_copy(c.id).await.unwrap().unwrap();
    assert_eq!(c2.places[0].missing_since, Some(10));
    assert!(
        engine
            .unmiss_place(PlaceKind::Path, "/Users/me/a.pdf")
            .await
            .unwrap()
    );
    assert!(
        !engine
            .unmiss_place(PlaceKind::Path, "/Users/me/a.pdf")
            .await
            .unwrap()
    );
    assert_eq!(
        engine.get_copy(c.id).await.unwrap().unwrap().places[0].missing_since,
        None
    );
    assert!(
        engine
            .copy_for_place(PlaceKind::Path, "/nowhere")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_shared_place_merges_copies_of_one_book_only() {
    let (_tmp, engine) = engine().await;
    let b = seed_book(&engine, "One").await;
    let other = seed_book(&engine, "Two").await;
    let keep = copy_of(&engine, b, None, Tenure::Unrecorded).await;
    let dup = copy_of(&engine, b, Some(true), Tenure::Own).await;
    engine
        .attach_place(dup.id, &NewPlace::new(PlaceKind::Calibre, "uuid-1"))
        .await
        .unwrap();
    let out = engine
        .attach_place(keep.id, &NewPlace::new(PlaceKind::Calibre, "uuid-1"))
        .await
        .unwrap();
    assert_eq!(out, PlaceAttach::Merged { absorbed: dup.id });
    let all = engine.copies_for_book(b).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, keep.id);
    assert_eq!(all[0].places.len(), 1);
    assert_eq!(
        all[0].tenure,
        Tenure::Own,
        "unrecorded yields to a recorded tenure"
    );
    assert_eq!(
        all[0].have_now,
        Some(true),
        "a gap is filled, never decided"
    );

    let theirs = copy_of(&engine, other, Some(true), Tenure::Own).await;
    let out = engine
        .attach_place(theirs.id, &NewPlace::new(PlaceKind::Calibre, "uuid-1"))
        .await
        .unwrap();
    assert_eq!(
        out,
        PlaceAttach::OtherBook {
            book_id: b,
            copy_id: keep.id
        }
    );
    assert!(engine.merge_copies(keep.id, theirs.id).await.is_err());
}

#[tokio::test]
async fn merging_books_carries_their_copies() {
    let (_tmp, engine) = engine().await;
    let dst = seed_book(&engine, "Keep").await;
    let src = seed_book(&engine, "Fold").await;
    let a = copy_of(&engine, dst, Some(true), Tenure::Own).await;
    let b = copy_of(&engine, src, Some(true), Tenure::Borrowed).await;
    engine
        .attach_place(b.id, &NewPlace::new(PlaceKind::Store, "sha"))
        .await
        .unwrap();
    engine.merge_books(src, dst).await.unwrap();

    let copies = engine.copies_for_book(dst).await.unwrap();
    assert_eq!(copies.len(), 2, "different copies stay different");
    assert!(copies.iter().any(|c| c.id == a.id));
    let moved = copies.iter().find(|c| c.id == b.id).unwrap();
    assert_eq!(moved.book_id, dst);
    assert_eq!(moved.places.len(), 1);
    assert!(engine.in_library(dst).await.unwrap());
}

#[tokio::test]
async fn the_library_scope_filters_the_list() {
    let (_tmp, engine) = engine().await;
    let held = seed_book(&engine, "Held").await;
    let gone_untouched = seed_book(&engine, "Gone").await;
    let gone_touched = seed_book(&engine, "Kept").await;
    let nothing = seed_book(&engine, "Nothing").await;
    copy_of(&engine, held, Some(true), Tenure::Own).await;
    copy_of(&engine, gone_untouched, Some(false), Tenure::Borrowed).await;
    copy_of(&engine, gone_touched, Some(false), Tenure::Borrowed).await;
    touch(&engine, gone_touched, Touch::Highlight).await;

    let ids = |books: Vec<bookmole::Book>| {
        let mut v: Vec<i64> = books.into_iter().map(|b| b.id.unwrap()).collect();
        v.sort();
        v
    };
    let q = |scope| {
        BookQuery::default().with_filter(BookFilter {
            library: scope,
            ..Default::default()
        })
    };
    assert_eq!(LibraryScope::default(), LibraryScope::All);
    assert_eq!(BookFilter::default().library, LibraryScope::All);

    let all = ids(engine.list_books(&q(LibraryScope::All)).await.unwrap());
    assert_eq!(all.len(), 4, "the default scope changes nothing");
    let inside = ids(engine
        .list_books(&q(LibraryScope::InLibrary))
        .await
        .unwrap());
    assert_eq!(inside, {
        let mut v = vec![held, gone_touched];
        v.sort();
        v
    });
    let outside = ids(engine
        .list_books(&q(LibraryScope::NotInLibrary))
        .await
        .unwrap());
    assert_eq!(outside, {
        let mut v = vec![gone_untouched, nothing];
        v.sort();
        v
    });
    for scope in [
        LibraryScope::All,
        LibraryScope::InLibrary,
        LibraryScope::NotInLibrary,
    ] {
        let f = BookFilter {
            library: scope,
            ..Default::default()
        };
        let n = engine.storage().count_books(&f).await.unwrap();
        let page = engine.list_books(&q(scope)).await.unwrap().len();
        assert_eq!(n as usize, page, "{scope:?}");
    }
}

#[tokio::test]
async fn a_frontend_save_that_creates_a_book_records_an_unrecorded_copy() {
    let (_tmp, engine) = engine().await;
    let book = bookmole::Book {
        title: Some("Middlemarch".into()),
        isbn_13: Some("9781900000178".into()),
        ..Default::default()
    };
    let saved = engine.save_book_unrecorded(&book).await.unwrap();
    let id = saved.id.unwrap();
    let copies = engine.copies_for_book(id).await.unwrap();
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].tenure, Tenure::Unrecorded);
    assert_eq!(copies[0].learned_from, LearnedFrom::User);
    assert_eq!(copies[0].have_now, None);
    assert!(engine.book_standing(id).await.unwrap().in_library);

    engine.delete_copy(copies[0].id).await.unwrap();
    engine.save_book_unrecorded(&book).await.unwrap();
    assert!(engine.copies_for_book(id).await.unwrap().is_empty());
}

#[tokio::test]
async fn copy_facts_say_format_keepability_and_the_readers_name() {
    use bookmole::HighlightOrigin;

    let (tmp, engine) = engine().await;
    let book = seed_book(&engine, "Facts").await;
    let copy = copy_of(&engine, book, Some(true), Tenure::Own).await;

    let facts = engine.copy_facts(&copy).await.unwrap();
    assert_eq!((facts.format, facts.can_keep_here), (None, false));

    let file = tmp.path().join("Facts.EPUB");
    std::fs::write(&file, b"not really an epub").unwrap();
    let path = file.display().to_string();
    engine
        .attach_place(copy.id, &NewPlace::new(PlaceKind::Path, path.clone()))
        .await
        .unwrap();
    let copy = engine.get_copy(copy.id).await.unwrap().unwrap();
    let facts = engine.copy_facts(&copy).await.unwrap();
    assert_eq!(facts.format.as_deref(), Some("epub"));
    assert!(facts.can_keep_here);

    engine
        .mark_place_missing(PlaceKind::Path, &path, 5)
        .await
        .unwrap();
    let missing = engine.get_copy(copy.id).await.unwrap().unwrap();
    assert!(!engine.copy_facts(&missing).await.unwrap().can_keep_here);
    engine.unmiss_place(PlaceKind::Path, &path).await.unwrap();

    engine.keep_copy_here(copy.id).await.unwrap();
    let kept = engine.get_copy(copy.id).await.unwrap().unwrap();
    let facts = engine.copy_facts(&kept).await.unwrap();
    assert!(!facts.can_keep_here);
    assert_eq!(facts.format.as_deref(), Some("epub"));

    engine
        .storage()
        .record_pairing("dev-1", Some("Kobo Libra"), "tok", 1, None, 0)
        .await
        .unwrap();
    let mut place = NewPlace::new(PlaceKind::Reader, "md5-x");
    place.detail = Some("dev-1".into());
    engine.attach_place(copy.id, &place).await.unwrap();
    let mut unknown = NewPlace::new(PlaceKind::Reader, "md5-y");
    unknown.detail = Some("not-paired".into());
    engine.attach_place(copy.id, &unknown).await.unwrap();
    let with_readers = engine.get_copy(copy.id).await.unwrap().unwrap();
    let facts = engine.copy_facts(&with_readers).await.unwrap();
    assert_eq!(
        facts.reader_devices,
        vec![("md5-x".to_string(), "Kobo Libra".to_string())]
    );

    let h = highlight("a line", "2026-01-01 10:00:00");
    engine.storage().insert_highlight(book, &h).await.unwrap();
    engine
        .storage()
        .stamp_highlight_copy(book, &h, copy.id)
        .await
        .unwrap();
    let origins = engine.highlight_origins(book).await.unwrap();
    assert_eq!(origins.len(), 1);
    assert_eq!(origins[0].origin, HighlightOrigin::Koreader);
    assert_eq!(origins[0].copy_id, Some(copy.id));
    assert_eq!(origins[0].device_name.as_deref(), Some("Kobo Libra"));
    assert!(origins[0].on_this_computer);
}

#[tokio::test]
async fn keeping_a_copy_with_no_file_is_invalid_input() {
    let (_tmp, engine) = engine().await;
    let book = seed_book(&engine, "No file").await;
    let copy = copy_of(&engine, book, Some(true), Tenure::Own).await;
    assert!(matches!(
        engine.keep_copy_here(copy.id).await,
        Err(bookmole::EngineError::InvalidInput(_))
    ));
}

#[tokio::test]
async fn copy_files_are_where_they_are_now() {
    use bookmole::PlaceFile;

    let (tmp, engine) = engine().await;
    let book = seed_book(&engine, "Files").await;
    let copy = copy_of(&engine, book, Some(true), Tenure::Own).await;
    let file = tmp.path().join("Files.epub");
    std::fs::write(&file, b"twelve bytes").unwrap();
    let path = file.display().to_string();
    engine
        .attach_place(copy.id, &NewPlace::new(PlaceKind::Path, path.clone()))
        .await
        .unwrap();
    engine
        .attach_place(copy.id, &NewPlace::new(PlaceKind::Reader, "md5-r"))
        .await
        .unwrap();

    let rows = engine.copy_files(book).await.unwrap();
    assert_eq!(rows.len(), 1, "a reader place is not a file row: {rows:?}");
    let r = &rows[0];
    assert_eq!(
        (r.copy_id, r.kind, r.reference.as_str()),
        (copy.id, PlaceKind::Path, path.as_str())
    );
    assert_eq!(r.path.as_deref(), Some(file.as_path()));
    assert_eq!(r.format.as_deref(), Some("epub"));
    assert_eq!((r.on_disk, r.size), (Some(true), Some(12)));
    assert_eq!(r.calibre_library, None);
    assert!(r.last_seen_at.is_some());

    engine.keep_copy_here(copy.id).await.unwrap();
    let rows = engine.copy_files(book).await.unwrap();
    let store = rows.iter().find(|r| r.kind == PlaceKind::Store).unwrap();
    assert_eq!((store.on_disk, store.size), (Some(true), Some(12)));
    assert_eq!(store.format.as_deref(), Some("epub"));
    assert!(store.path.as_ref().unwrap().starts_with(tmp.path()));

    std::fs::remove_file(&file).unwrap();
    let rows = engine.copy_files(book).await.unwrap();
    let gone = rows.iter().find(|r| r.kind == PlaceKind::Path).unwrap();
    assert_eq!(
        (gone.on_disk, gone.size, gone.missing_since),
        (Some(false), None, None)
    );

    let cal = copy_of(&engine, book, None, Tenure::Own).await;
    let mut place = NewPlace::new(PlaceKind::Calibre, "uuid-1");
    place.detail = Some("/Books/Calibre\tepub,azw3".into());
    engine.attach_place(cal.id, &place).await.unwrap();
    let rows = engine.copy_files(book).await.unwrap();
    let bare: Vec<_> = rows
        .iter()
        .filter(|r| r.kind == PlaceKind::Calibre)
        .collect();
    assert_eq!(bare.len(), 1);
    assert_eq!(
        (
            bare[0].path.as_ref(),
            bare[0].on_disk,
            bare[0].format.as_ref()
        ),
        (None, None, None)
    );
    assert_eq!(bare[0].calibre_library.as_deref(), Some("/Books/Calibre"));

    let azw3 = tmp.path().join("Files.azw3");
    std::fs::write(&azw3, b"azw").unwrap();
    let files = [
        PlaceFile {
            format: "EPUB".into(),
            path: tmp.path().join("not-there.epub"),
        },
        PlaceFile {
            format: "azw3".into(),
            path: azw3.clone(),
        },
    ];
    assert!(
        engine
            .storage()
            .set_place_files(PlaceKind::Calibre, "uuid-1", &files)
            .await
            .unwrap()
    );
    let rows = engine.copy_files(book).await.unwrap();
    let cal_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.kind == PlaceKind::Calibre)
        .collect();
    assert_eq!(cal_rows.len(), 2);
    assert_eq!(cal_rows[0].format.as_deref(), Some("azw3"));
    assert_eq!(
        (cal_rows[0].on_disk, cal_rows[0].size),
        (Some(true), Some(3))
    );
    assert_eq!(cal_rows[1].format.as_deref(), Some("epub"), "lowercased");
    assert_eq!((cal_rows[1].on_disk, cal_rows[1].size), (Some(false), None));
    assert!(cal_rows.iter().all(|r| r.copy_id == cal.id));
    let order: Vec<i64> = rows.iter().map(|r| r.copy_id).collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted);

    assert!(matches!(
        engine.copy_files(9_999).await,
        Err(bookmole::EngineError::NotFound(_))
    ));
}

#[tokio::test]
async fn place_files_travel_with_their_place() {
    use bookmole::PlaceFile;

    let (_tmp, engine) = engine().await;
    let s = engine.storage();
    let book = seed_book(&engine, "Merge").await;
    let a = copy_of(&engine, book, None, Tenure::Own).await;
    let b = copy_of(&engine, book, None, Tenure::Own).await;
    engine
        .attach_place(b.id, &NewPlace::new(PlaceKind::Calibre, "uuid-m"))
        .await
        .unwrap();
    let files = [PlaceFile {
        format: "epub".into(),
        path: "/lib/Merge.epub".into(),
    }];
    s.set_place_files(PlaceKind::Calibre, "uuid-m", &files)
        .await
        .unwrap();
    assert!(
        !s.set_place_files(PlaceKind::Calibre, "no-such-place", &files)
            .await
            .unwrap()
    );

    engine.merge_copies(a.id, b.id).await.unwrap();
    let rows = engine.copy_files(book).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].copy_id, a.id);
    assert_eq!(
        rows[0].path.as_deref(),
        Some(std::path::Path::new("/lib/Merge.epub"))
    );

    s.delete_place(PlaceKind::Calibre, "uuid-m").await.unwrap();
    engine
        .attach_place(a.id, &NewPlace::new(PlaceKind::Calibre, "uuid-m"))
        .await
        .unwrap();
    assert!(
        s.place_files(PlaceKind::Calibre, "uuid-m")
            .await
            .unwrap()
            .is_empty(),
        "the old rows went with the deleted place"
    );
    s.set_place_files(PlaceKind::Calibre, "uuid-m", &files)
        .await
        .unwrap();
    engine.delete_copy(a.id).await.unwrap();
    assert!(
        s.place_files(PlaceKind::Calibre, "uuid-m")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn highlight_sources_group_by_copy_and_origin() {
    use bookmole::{HighlightOrigin, ImportedHighlight};

    let (_tmp, engine) = engine().await;
    let s = engine.storage();
    let book = seed_book(&engine, "Sources").await;
    let copy = copy_of(&engine, book, Some(true), Tenure::Own).await;
    s.record_pairing("dev-1", Some("Kobo Libra"), "tok", 1, None, 0)
        .await
        .unwrap();
    let mut reader = NewPlace::new(PlaceKind::Reader, "md5-s");
    reader.detail = Some("dev-1".into());
    engine.attach_place(copy.id, &reader).await.unwrap();

    for (i, text) in ["one", "two"].iter().enumerate() {
        let h = highlight(text, &format!("2026-01-0{} 10:00:00", i + 1));
        s.insert_highlight(book, &h).await.unwrap();
        s.stamp_highlight_copy(book, &h, copy.id).await.unwrap();
    }
    let mut kindle = ImportedHighlight::new(HighlightOrigin::Kindle, "k-1", "clipped");
    kindle.copy_id = Some(copy.id);
    s.upsert_imported_highlight(book, &kindle).await.unwrap();
    engine
        .create_highlight(book, "typed by hand", None, None)
        .await
        .unwrap();

    let sources = engine.copy_highlight_sources(book).await.unwrap();
    let seen = engine.get_copy(copy.id).await.unwrap().unwrap().places[0].last_seen_at;
    assert_eq!(sources.len(), 3, "{sources:?}");
    assert_eq!(
        (sources[0].copy_id, sources[0].origin),
        (None, HighlightOrigin::User)
    );
    assert_eq!(
        (sources[0].device_name.as_ref(), sources[0].last_read_at),
        (None, None)
    );
    assert_eq!(
        (sources[1].copy_id, sources[1].origin),
        (Some(copy.id), HighlightOrigin::Kindle)
    );
    assert_eq!(sources[1].last_read_at, None);
    assert_eq!(sources[1].device_name.as_deref(), Some("Kobo Libra"));
    assert_eq!(
        (sources[2].copy_id, sources[2].origin),
        (Some(copy.id), HighlightOrigin::Koreader)
    );
    assert_eq!(sources[2].device_name.as_deref(), Some("Kobo Libra"));
    assert!(seen.is_some());
    assert_eq!(sources[2].last_read_at, seen);

    assert!(matches!(
        engine.copy_highlight_sources(9_999).await,
        Err(bookmole::EngineError::NotFound(_))
    ));
}

#[tokio::test]
async fn finding_a_moved_file_repoints_the_copy_and_keeps_calibre() {
    let (tmp, engine) = engine().await;
    let book = seed_book(&engine, "Moved").await;
    let copy = copy_of(&engine, book, None, Tenure::Own).await;
    let old = tmp.path().join("old/Moved.epub");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, b"the same bytes").unwrap();
    let old_ref = old.display().to_string();
    engine
        .attach_place(copy.id, &NewPlace::new(PlaceKind::Path, old_ref.clone()))
        .await
        .unwrap();
    let mut cal = NewPlace::new(PlaceKind::Calibre, "uuid-moved");
    cal.detail = Some("\tepub".into());
    engine.attach_place(copy.id, &cal).await.unwrap();

    let new = tmp.path().join("new/Moved.epub");
    std::fs::create_dir_all(new.parent().unwrap()).unwrap();
    std::fs::rename(&old, &new).unwrap();

    let found = engine.relocate_copy_file(copy.id, &new).await.unwrap();
    assert_eq!(found.id, copy.id);
    assert_eq!(found.have_now, None, "only the user changes have_now");
    let kinds: Vec<(PlaceKind, String)> = found
        .places
        .iter()
        .map(|p| (p.kind, p.reference.clone()))
        .collect();
    assert!(kinds.contains(&(PlaceKind::Calibre, "uuid-moved".into())));
    assert!(kinds.contains(&(PlaceKind::Path, new.display().to_string())));
    assert!(!kinds.iter().any(|(_, r)| *r == old_ref), "{kinds:?}");
    let place = found
        .places
        .iter()
        .find(|p| p.kind == PlaceKind::Path)
        .unwrap();
    assert_eq!(
        place.last_hash.as_deref(),
        Some(bookmole::files::sha256_of(&new).unwrap().as_str())
    );
    assert!(place.last_mtime.is_some());
    assert_eq!(engine.copies_for_book(book).await.unwrap().len(), 1);

    let again = engine.relocate_copy_file(copy.id, &new).await.unwrap();
    assert_eq!(again.places.len(), found.places.len());
}

#[tokio::test]
async fn finding_the_file_refuses_what_is_not_this_copys() {
    use bookmole::EngineError;

    let (tmp, engine) = engine().await;
    let book = seed_book(&engine, "Mine").await;
    let copy = copy_of(&engine, book, None, Tenure::Own).await;
    let other = seed_book(&engine, "Theirs").await;
    let theirs = copy_of(&engine, other, None, Tenure::Own).await;
    let file = tmp.path().join("theirs.epub");
    std::fs::write(&file, b"their bytes").unwrap();
    engine
        .attach_place(
            theirs.id,
            &NewPlace::new(PlaceKind::Path, file.display().to_string()),
        )
        .await
        .unwrap();

    let r = engine.relocate_copy_file(9_999, &file).await;
    assert!(matches!(r, Err(EngineError::NotFound(_))), "{r:?}");
    let r = engine
        .relocate_copy_file(copy.id, &tmp.path().join("nope.epub"))
        .await;
    assert!(matches!(r, Err(EngineError::NotFound(_))), "{r:?}");
    let r = engine.relocate_copy_file(copy.id, tmp.path()).await;
    assert!(matches!(r, Err(EngineError::InvalidInput(_))), "{r:?}");
    let r = engine.relocate_copy_file(copy.id, &file).await;
    assert!(matches!(r, Err(EngineError::InvalidInput(_))), "{r:?}");

    engine.keep_copy_here(theirs.id).await.unwrap();
    let elsewhere = tmp.path().join("elsewhere.epub");
    std::fs::write(&elsewhere, b"their bytes").unwrap();
    let r = engine.relocate_copy_file(copy.id, &elsewhere).await;
    assert!(matches!(r, Err(EngineError::InvalidInput(_))), "{r:?}");
    assert!(
        engine
            .get_copy(copy.id)
            .await
            .unwrap()
            .unwrap()
            .places
            .is_empty(),
        "a refusal writes nothing"
    );
}
