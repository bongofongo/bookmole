//! The shared write path for non-KOReader highlight importers.

mod common;
use bookmole::highlight_origin::IdentityScope;
use bookmole::storage::imported_identity_hash;
use bookmole::{
    CopyForm, HighlightOrigin, ImportedHighlight, ImportedOutcome, LearnedFrom, NewCopy, PosKind,
    Tenure,
};
use common::{engine, highlight, seed_book};

fn imp(origin: HighlightOrigin, id: Option<&str>, identity: &str, text: &str) -> ImportedHighlight {
    let mut h = ImportedHighlight::new(origin, identity, text);
    h.origin_id = id.map(str::to_string);
    h
}

fn scoped(id: Option<&str>, identity: &str, text: &str, copy: Option<i64>) -> ImportedHighlight {
    let mut h = imp(HighlightOrigin::Preview, id, identity, text);
    h.identity_scope = IdentityScope::Copy;
    h.copy_id = copy;
    h
}

async fn new_copy(s: &bookmole::storage::Storage, book: i64) -> i64 {
    s.create_copy(&NewCopy::new(
        book,
        Some(CopyForm::File),
        Tenure::Own,
        LearnedFrom::User,
    ))
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn insert_then_identical_import_is_unchanged() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let mut h = imp(HighlightOrigin::Preview, Some("nm-1"), "x", "words");
    h.origin_note = Some("note".into());
    h.color = Some("#ffff00".into());
    h.page = Some(4);
    h.pos_kind = Some(PosKind::PdfRect);
    h.pos0 = Some("p4:h:1,2".into());
    h.made_at = Some("2026-01-01 00:00:00".into());

    let ImportedOutcome::Inserted(id) = s.upsert_imported_highlight(book, &h).await.unwrap() else {
        panic!("first import inserts");
    };
    assert_eq!(
        s.upsert_imported_highlight(book, &h).await.unwrap(),
        ImportedOutcome::Unchanged(id)
    );
    let rows = s
        .imported_highlights(book, HighlightOrigin::Preview)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.origin, HighlightOrigin::Preview);
    assert_eq!(r.source, "preview");
    assert_eq!(r.origin_id.as_deref(), Some("nm-1"));
    assert_eq!(r.origin_note.as_deref(), Some("note"));
    assert_eq!(r.pos_kind, Some(PosKind::PdfRect));
    assert_eq!(r.page, Some(4));
    assert_eq!(r.ko_datetime.as_deref(), Some("2026-01-01 00:00:00"));
}

#[tokio::test]
async fn the_source_owns_its_fields_and_we_own_the_annotation() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let mut h = imp(HighlightOrigin::Preview, Some("nm-1"), "x", "words");
    h.origin_note = Some("first".into());
    let id = s.upsert_imported_highlight(book, &h).await.unwrap().id();
    s.set_annotation(id, Some("what I think")).await.unwrap();

    h.origin_note = Some("second".into());
    h.color = Some("#00ff00".into());
    h.text = "other words".into();
    assert_eq!(
        s.upsert_imported_highlight(book, &h).await.unwrap(),
        ImportedOutcome::Updated(id)
    );
    let r = s.get_highlight(id).await.unwrap().unwrap();
    assert_eq!(r.origin_note.as_deref(), Some("second"));
    assert_eq!(r.text, "other words");
    assert_eq!(r.annotation.as_deref(), Some("what I think"));

    h.origin_note = None;
    assert_eq!(
        s.upsert_imported_highlight(book, &h).await.unwrap(),
        ImportedOutcome::Updated(id)
    );
    let r = s.get_highlight(id).await.unwrap().unwrap();
    assert_eq!(r.origin_note, None);
    assert_eq!(r.annotation.as_deref(), Some("what I think"));
}

#[tokio::test]
async fn identity_is_used_when_there_is_no_origin_id() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let h = imp(HighlightOrigin::Kindle, None, "loc 10-12", "a line");
    let id = s.upsert_imported_highlight(book, &h).await.unwrap().id();
    assert_eq!(
        s.upsert_imported_highlight(book, &h).await.unwrap(),
        ImportedOutcome::Unchanged(id)
    );
    let other = imp(HighlightOrigin::Kindle, None, "loc 20-22", "a line");
    assert!(matches!(
        s.upsert_imported_highlight(book, &other).await.unwrap(),
        ImportedOutcome::Inserted(_)
    ));
}

#[tokio::test]
async fn two_origins_with_the_same_key_never_collide() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let a = imp(HighlightOrigin::Preview, Some("same"), "same", "t");
    let b = imp(HighlightOrigin::AppleBooks, Some("same"), "same", "t");
    assert_ne!(imported_identity_hash(&a), imported_identity_hash(&b));
    let (ia, ib) = (
        s.upsert_imported_highlight(book, &a).await.unwrap().id(),
        s.upsert_imported_highlight(book, &b).await.unwrap().id(),
    );
    assert_ne!(ia, ib, "the same passage from two apps is two highlights");
    let c = imp(HighlightOrigin::Preview, None, "same", "t");
    assert_ne!(imported_identity_hash(&a), imported_identity_hash(&c));
}

#[tokio::test]
async fn an_imported_hash_never_equals_koreaders() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let ko = highlight("same words", "2026-01-01 00:00:00");
    s.insert_highlight(book, &ko).await.unwrap().unwrap();
    let h = imp(
        HighlightOrigin::Preview,
        None,
        &format!("{book}|2026-01-01 00:00:00||same words"),
        "same words",
    );
    assert_ne!(imported_identity_hash(&h), ko.identity_hash());
    s.upsert_imported_highlight(book, &h).await.unwrap();
    assert_eq!(s.list_highlights(book).await.unwrap().len(), 2);
}

#[tokio::test]
async fn koreader_and_user_are_not_importable() {
    let (_t, e) = engine().await;
    let book = seed_book(&e, "A").await;
    for o in [HighlightOrigin::Koreader, HighlightOrigin::User] {
        assert!(
            e.storage()
                .upsert_imported_highlight(book, &imp(o, None, "x", "t"))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn the_copy_is_stamped_only_while_null() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let mk = |s: &bookmole::storage::Storage| {
        let s = s.clone();
        async move {
            s.create_copy(&NewCopy::new(
                book,
                Some(CopyForm::File),
                Tenure::Own,
                LearnedFrom::User,
            ))
            .await
            .unwrap()
            .id
        }
    };
    let (c1, c2) = (mk(s).await, mk(s).await);
    let mut h = imp(HighlightOrigin::Preview, Some("n"), "x", "t");
    let id = s.upsert_imported_highlight(book, &h).await.unwrap().id();
    assert_eq!(s.get_highlight(id).await.unwrap().unwrap().copy_id, None);

    h.copy_id = Some(c1);
    assert_eq!(
        s.upsert_imported_highlight(book, &h).await.unwrap(),
        ImportedOutcome::Unchanged(id)
    );
    assert_eq!(
        s.get_highlight(id).await.unwrap().unwrap().copy_id,
        Some(c1)
    );
    h.copy_id = Some(c2);
    s.upsert_imported_highlight(book, &h).await.unwrap();
    assert_eq!(
        s.get_highlight(id).await.unwrap().unwrap().copy_id,
        Some(c1)
    );
}

#[tokio::test]
async fn the_search_index_follows_without_being_written() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let mut h = imp(
        HighlightOrigin::Preview,
        Some("n"),
        "x",
        "a quixotic passage",
    );
    h.origin_note = Some("zeitgeist".into());
    s.upsert_imported_highlight(book, &h).await.unwrap();
    let hits = |q: &'static str| {
        let s = s.clone();
        async move { s.search_marks(q, None, None, 10).await.unwrap().len() }
    };
    assert_eq!(hits("quixotic").await, 1);
    assert_eq!(hits("zeitgeist").await, 1);
    h.origin_note = Some("different".into());
    s.upsert_imported_highlight(book, &h).await.unwrap();
    assert_eq!(hits("zeitgeist").await, 0);
    assert_eq!(hits("different").await, 1);
}

#[tokio::test]
async fn removal_spares_anything_of_ours() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let plain = s
        .upsert_imported_highlight(book, &imp(HighlightOrigin::Preview, Some("a"), "a", "t"))
        .await
        .unwrap()
        .id();
    let annotated = s
        .upsert_imported_highlight(book, &imp(HighlightOrigin::Preview, Some("b"), "b", "t"))
        .await
        .unwrap()
        .id();
    let carded = s
        .upsert_imported_highlight(book, &imp(HighlightOrigin::Preview, Some("c"), "c", "t"))
        .await
        .unwrap()
        .id();
    s.set_annotation(annotated, Some("mine")).await.unwrap();
    s.insert_flashcard(book, Some(carded), "word", None)
        .await
        .unwrap();

    assert!(!s.imported_highlight_kept(plain).await.unwrap());
    assert!(s.imported_highlight_kept(annotated).await.unwrap());
    assert!(s.imported_highlight_kept(carded).await.unwrap());

    let p = HighlightOrigin::Preview;
    assert!(s.remove_imported_highlight(book, p, "a").await.unwrap());
    assert!(
        !s.remove_imported_highlight(book, p, "a").await.unwrap(),
        "already gone"
    );
    assert!(!s.remove_imported_highlight(book, p, "b").await.unwrap());
    assert!(!s.remove_imported_highlight(book, p, "c").await.unwrap());
    assert_eq!(s.imported_highlights(book, p).await.unwrap().len(), 2);
    assert_eq!(s.list_flashcards_for_book(book).await.unwrap().len(), 1);
}

#[tokio::test]
async fn removal_never_reaches_koreader_or_user_rows() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let id = s
        .insert_highlight(book, &highlight("x", "2026-01-01 00:00:00"))
        .await
        .unwrap()
        .unwrap();
    assert!(!s.remove_imported_highlight_row(id).await.unwrap());
    assert!(s.get_highlight(id).await.unwrap().is_some());
}

#[tokio::test]
async fn a_merged_book_keeps_its_imported_identity() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let (src, dst) = (seed_book(&e, "A").await, seed_book(&e, "B").await);
    let h = imp(HighlightOrigin::Kindle, None, "loc 1", "t");
    let id = s.upsert_imported_highlight(src, &h).await.unwrap().id();
    s.merge_books(src, dst).await.unwrap();
    assert_eq!(s.get_highlight(id).await.unwrap().unwrap().book_id, dst);
    assert_eq!(
        s.upsert_imported_highlight(dst, &h).await.unwrap(),
        ImportedOutcome::Unchanged(id)
    );
}

#[test]
fn new_says_nothing_but_what_it_is_given() {
    let h = ImportedHighlight::new(HighlightOrigin::Kindle, "loc 1", "words");
    assert_eq!(h.identity_scope, IdentityScope::Global);
    assert_eq!((h.copy_id, h.origin_id.as_deref()), (None, None));
    assert_eq!((h.identity.as_str(), h.text.as_str()), ("loc 1", "words"));
    assert!(!h.text_unavailable);
    assert_eq!(IdentityScope::default(), IdentityScope::Global);
}

#[tokio::test]
async fn a_copy_scoped_key_is_one_highlight_per_copy() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let (c1, c2) = (new_copy(s, book).await, new_copy(s, book).await);

    for (id, identity) in [(Some("nm-1"), "x"), (None, "1|h|144,697,209,712")] {
        let a = s
            .upsert_imported_highlight(book, &scoped(id, identity, "in a", Some(c1)))
            .await
            .unwrap();
        let b = s
            .upsert_imported_highlight(book, &scoped(id, identity, "in b", Some(c2)))
            .await
            .unwrap();
        assert!(matches!(a, ImportedOutcome::Inserted(_)), "{id:?}");
        assert!(matches!(b, ImportedOutcome::Inserted(_)), "{id:?}");
        assert_ne!(a.id(), b.id());

        let mut edited = scoped(id, identity, "in a", Some(c1));
        edited.origin_note = Some("edited in a".into());
        assert_eq!(
            s.upsert_imported_highlight(book, &edited).await.unwrap(),
            ImportedOutcome::Updated(a.id())
        );
        assert_eq!(
            s.upsert_imported_highlight(book, &scoped(id, identity, "in b", Some(c2)))
                .await
                .unwrap(),
            ImportedOutcome::Unchanged(b.id())
        );
        let (ra, rb) = (
            s.get_highlight(a.id()).await.unwrap().unwrap(),
            s.get_highlight(b.id()).await.unwrap().unwrap(),
        );
        assert_eq!(
            (ra.text.as_str(), ra.origin_note.as_deref(), ra.copy_id),
            ("in a", Some("edited in a"), Some(c1))
        );
        assert_eq!(
            (rb.text.as_str(), rb.origin_note.as_deref(), rb.copy_id),
            ("in b", None, Some(c2))
        );
    }
    assert_eq!(
        s.imported_highlights(book, HighlightOrigin::Preview)
            .await
            .unwrap()
            .len(),
        4
    );
}

#[tokio::test]
async fn removal_in_one_copy_never_reaches_another() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let (c1, c2) = (new_copy(s, book).await, new_copy(s, book).await);
    let p = HighlightOrigin::Preview;
    let a = s
        .upsert_imported_highlight(book, &scoped(Some("nm"), "x", "t", Some(c1)))
        .await
        .unwrap()
        .id();
    let b = s
        .upsert_imported_highlight(book, &scoped(Some("nm"), "x", "t", Some(c2)))
        .await
        .unwrap()
        .id();

    assert!(!s.remove_imported_highlight(book, p, "nm").await.unwrap());
    assert_eq!(s.imported_highlights(book, p).await.unwrap().len(), 2);

    assert!(
        s.remove_imported_highlight_in_copy(book, p, c2, "nm")
            .await
            .unwrap()
    );
    assert!(s.get_highlight(b).await.unwrap().is_none());
    assert!(s.get_highlight(a).await.unwrap().is_some());
    assert!(
        !s.remove_imported_highlight_in_copy(book, p, c2, "nm")
            .await
            .unwrap(),
        "already gone"
    );
    s.set_annotation(a, Some("mine")).await.unwrap();
    assert!(
        !s.remove_imported_highlight_in_copy(book, p, c1, "nm")
            .await
            .unwrap()
    );
    assert!(s.get_highlight(a).await.unwrap().is_some());
}

#[tokio::test]
async fn global_keys_match_whatever_the_copy() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let (c1, c2) = (new_copy(s, book).await, new_copy(s, book).await);

    for mut h in [
        imp(HighlightOrigin::AppleBooks, Some("UUID-1"), "x", "t"),
        imp(HighlightOrigin::Kindle, None, "Title|loc 10-12|t", "t"),
    ] {
        let id = s.upsert_imported_highlight(book, &h).await.unwrap().id();
        for copy in [Some(c1), Some(c2), None] {
            h.copy_id = copy;
            assert_eq!(
                s.upsert_imported_highlight(book, &h).await.unwrap(),
                ImportedOutcome::Unchanged(id),
                "{:?} {copy:?}",
                h.origin
            );
        }
        let r = s.get_highlight(id).await.unwrap().unwrap();
        assert_eq!(r.copy_id, Some(c1));
        assert_eq!(
            s.imported_highlights(book, h.origin).await.unwrap().len(),
            1
        );
    }
    assert!(
        s.remove_imported_highlight(book, HighlightOrigin::AppleBooks, "UUID-1")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_row_with_no_copy_is_claimed_by_the_first_copy_to_import_it() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let (c1, c2) = (new_copy(s, book).await, new_copy(s, book).await);
    for id in [Some("nm"), None] {
        let a = s
            .upsert_imported_highlight(book, &scoped(id, "1|h|1,2,3,4", "t", None))
            .await
            .unwrap();
        assert!(matches!(a, ImportedOutcome::Inserted(_)));
        assert_eq!(
            s.get_highlight(a.id()).await.unwrap().unwrap().copy_id,
            None
        );

        assert_eq!(
            s.upsert_imported_highlight(book, &scoped(id, "1|h|1,2,3,4", "t", Some(c1)))
                .await
                .unwrap(),
            ImportedOutcome::Unchanged(a.id())
        );
        assert_eq!(
            s.get_highlight(a.id()).await.unwrap().unwrap().copy_id,
            Some(c1)
        );
        let b = s
            .upsert_imported_highlight(book, &scoped(id, "1|h|1,2,3,4", "t", Some(c2)))
            .await
            .unwrap();
        assert!(matches!(b, ImportedOutcome::Inserted(_)));
        assert_eq!(
            s.upsert_imported_highlight(book, &scoped(id, "1|h|1,2,3,4", "t", None))
                .await
                .unwrap(),
            ImportedOutcome::Unchanged(a.id())
        );
    }
}

#[tokio::test]
async fn merged_copies_re_import_without_duplicating() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let (c1, c2) = (new_copy(s, book).await, new_copy(s, book).await);
    let file = |copy: i64, own: &str, words: &str| {
        vec![
            scoped(Some(own), "own", words, Some(copy)),
            scoped(None, &format!("1|h|{own}"), words, Some(copy)),
            scoped(Some("shared-nm"), "x", words, Some(copy)),
            scoped(None, "1|h|shared-place", words, Some(copy)),
        ]
    };
    let mut ids = Vec::new();
    for h in file(c1, "a-only", "A")
        .into_iter()
        .chain(file(c2, "b-only", "B"))
    {
        ids.push(s.upsert_imported_highlight(book, &h).await.unwrap().id());
    }
    assert_eq!(ids.len(), 8);

    s.merge_copies(c1, c2).await.unwrap();
    let p = HighlightOrigin::Preview;
    assert!(
        s.imported_highlights(book, p)
            .await
            .unwrap()
            .iter()
            .all(|r| r.copy_id == Some(c1))
    );

    for (words, own, from) in [("B", "b-only", 4), ("A", "a-only", 0)] {
        for (i, h) in file(c1, own, words).into_iter().enumerate() {
            assert_eq!(
                s.upsert_imported_highlight(book, &h).await.unwrap(),
                ImportedOutcome::Unchanged(ids[from + i]),
                "{words} #{i}"
            );
        }
    }
    assert_eq!(s.imported_highlights(book, p).await.unwrap().len(), 8);

    // Known limit: once a shared key differs from both rows, the oldest row (file A's) is refreshed.
    let mut changed = scoped(None, "1|h|shared-place", "B", Some(c1));
    changed.origin_note = Some("new in B".into());
    assert_eq!(
        s.upsert_imported_highlight(book, &changed).await.unwrap(),
        ImportedOutcome::Updated(ids[3])
    );
    assert_eq!(s.imported_highlights(book, p).await.unwrap().len(), 8);
}

#[tokio::test]
async fn a_new_copy_never_inherits_another_copys_rows() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    let (c1, c2) = (new_copy(s, book).await, new_copy(s, book).await);
    let a = s
        .upsert_imported_highlight(book, &scoped(None, "k", "a", Some(c1)))
        .await
        .unwrap()
        .id();
    let b = s
        .upsert_imported_highlight(book, &scoped(None, "k", "b", Some(c2)))
        .await
        .unwrap()
        .id();
    s.merge_copies(c1, c2).await.unwrap();
    let c3 = new_copy(s, book).await;
    let c = s
        .upsert_imported_highlight(book, &scoped(None, "k", "c", Some(c3)))
        .await
        .unwrap();
    assert!(matches!(c, ImportedOutcome::Inserted(_)));
    assert_eq!(s.get_highlight(a).await.unwrap().unwrap().text, "a");
    assert_eq!(s.get_highlight(b).await.unwrap().unwrap().text, "b");
}

#[tokio::test]
async fn one_key_fits_sixteen_copies_and_no_more() {
    let (_t, e) = engine().await;
    let s = e.storage();
    let book = seed_book(&e, "A").await;
    for n in 0..16 {
        let c = new_copy(s, book).await;
        assert!(
            matches!(
                s.upsert_imported_highlight(book, &scoped(None, "k", "t", Some(c)))
                    .await
                    .unwrap(),
                ImportedOutcome::Inserted(_)
            ),
            "copy {n}"
        );
    }
    let c = new_copy(s, book).await;
    assert!(matches!(
        s.upsert_imported_highlight(book, &scoped(None, "k", "t", Some(c)))
            .await,
        Err(bookmole::EngineError::InvalidInput(_))
    ));
}
