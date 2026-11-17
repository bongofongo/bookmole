//! Apple Books import tests over fixture databases built with Apple's table and column names.
//! Offline.

mod common;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use bookmole::apple_books::{ANNOTATION_DIR, CORE_DATA_EPOCH_UNIX, LIBRARY_DIR, STORE_ID_SOURCE};
use bookmole::{
    AppleBooksReport, AssetMatch, AssetOutcome, Book, CopyForm, DiagnosticKind, FileImportOptions,
    FileMatch, HighlightOrigin, LearnedFrom, PlaceKind, PosKind, Tenure, import_apple_books,
    link_apple_books_asset,
};
use common::{engine, write_isbnless_epub_by};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use sqlx::{Connection, SqliteConnection};

const LIBRARY_DDL: &str = "CREATE TABLE ZBKLIBRARYASSET (
    Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER, ZCONTENTTYPE INTEGER,
    ZASSETID VARCHAR, ZAUTHOR VARCHAR, ZPATH VARCHAR, ZSTOREID VARCHAR, ZTITLE VARCHAR)";

const ANNOTATION_DDL: &str = "CREATE TABLE ZAEANNOTATION (
    Z_PK INTEGER PRIMARY KEY, Z_ENT INTEGER,
    ZANNOTATIONDELETED INTEGER, ZANNOTATIONSTYLE INTEGER, ZANNOTATIONTYPE INTEGER,
    ZANNOTATIONCREATIONDATE TIMESTAMP, ZANNOTATIONMODIFICATIONDATE TIMESTAMP,
    ZANNOTATIONASSETID VARCHAR, ZANNOTATIONLOCATION VARCHAR, ZANNOTATIONNOTE VARCHAR,
    ZANNOTATIONSELECTEDTEXT VARCHAR, ZANNOTATIONUUID VARCHAR)";

const ANNOTATION_DDL_OLD: &str = "CREATE TABLE ZAEANNOTATION (
    Z_PK INTEGER PRIMARY KEY, ZANNOTATIONDELETED INTEGER,
    ZANNOTATIONCREATIONDATE TIMESTAMP, ZANNOTATIONASSETID VARCHAR,
    ZANNOTATIONLOCATION VARCHAR, ZANNOTATIONNOTE VARCHAR,
    ZANNOTATIONSELECTEDTEXT VARCHAR, ZANNOTATIONUUID VARCHAR)";

#[derive(Clone)]
struct Asset {
    id: String,
    title: Option<String>,
    author: Option<String>,
    path: Option<String>,
    store_id: Option<String>,
}

fn store_asset(id: &str, title: &str, author: &str, store_id: &str) -> Asset {
    Asset {
        id: id.into(),
        title: Some(title.into()),
        author: Some(author.into()),
        path: None,
        store_id: Some(store_id.into()),
    }
}

#[derive(Clone)]
struct Ann {
    uuid: String,
    asset: String,
    text: Option<String>,
    note: Option<String>,
    style: i64,
    kind: i64,
    deleted: i64,
    created: f64,
    location: String,
}

const WHEN: f64 = (1_767_647_648 - CORE_DATA_EPOCH_UNIX) as f64;
const WHEN_TEXT: &str = "2026-01-05 21:14:08";

fn hl(uuid: &str, asset: &str, text: &str) -> Ann {
    Ann {
        uuid: uuid.into(),
        asset: asset.into(),
        text: Some(text.into()),
        note: None,
        style: 3,
        kind: 2,
        deleted: 0,
        created: WHEN,
        location: format!("epubcfi(/6/4[{uuid}]!/4/2/1:0)"),
    }
}

async fn connect(path: &Path, wal: bool) -> SqliteConnection {
    let mode = if wal {
        SqliteJournalMode::Wal
    } else {
        SqliteJournalMode::Delete
    };
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(mode);
    SqliteConnection::connect_with(&opts).await.unwrap()
}

async fn insert_assets(conn: &mut SqliteConnection, assets: &[Asset]) {
    for a in assets {
        sqlx::query(
            "INSERT INTO ZBKLIBRARYASSET (ZASSETID, ZTITLE, ZAUTHOR, ZPATH, ZSTOREID, ZCONTENTTYPE)
             VALUES (?, ?, ?, ?, ?, 1)",
        )
        .bind(&a.id)
        .bind(&a.title)
        .bind(&a.author)
        .bind(&a.path)
        .bind(&a.store_id)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
}

async fn insert_annotations(conn: &mut SqliteConnection, anns: &[Ann]) {
    for a in anns {
        sqlx::query(
            "INSERT INTO ZAEANNOTATION (ZANNOTATIONUUID, ZANNOTATIONASSETID,
                 ZANNOTATIONSELECTEDTEXT, ZANNOTATIONNOTE, ZANNOTATIONSTYLE, ZANNOTATIONTYPE,
                 ZANNOTATIONDELETED, ZANNOTATIONCREATIONDATE, ZANNOTATIONMODIFICATIONDATE,
                 ZANNOTATIONLOCATION)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&a.uuid)
        .bind(&a.asset)
        .bind(&a.text)
        .bind(&a.note)
        .bind(a.style)
        .bind(a.kind)
        .bind(a.deleted)
        .bind(a.created)
        .bind(a.created)
        .bind(&a.location)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
}

fn library_path(root: &Path, suffix: &str) -> PathBuf {
    let dir = root.join(LIBRARY_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("BKLibrary-{suffix}.sqlite"))
}

fn annotation_path(root: &Path, suffix: &str) -> PathBuf {
    let dir = root.join(ANNOTATION_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("AEAnnotation_{suffix}_local.sqlite"))
}

async fn write_library(root: &Path, assets: &[Asset]) -> PathBuf {
    let path = library_path(root, "1-091020131601");
    let mut conn = connect(&path, false).await;
    sqlx::raw_sql(LIBRARY_DDL).execute(&mut conn).await.unwrap();
    insert_assets(&mut conn, assets).await;
    conn.close().await.unwrap();
    path
}

async fn write_annotations(root: &Path, anns: &[Ann]) -> PathBuf {
    write_annotations_named(root, "v10312011_1727", anns).await
}

async fn write_annotations_named(root: &Path, suffix: &str, anns: &[Ann]) -> PathBuf {
    let path = annotation_path(root, suffix);
    let mut conn = connect(&path, false).await;
    sqlx::raw_sql(ANNOTATION_DDL)
        .execute(&mut conn)
        .await
        .unwrap();
    insert_annotations(&mut conn, anns).await;
    conn.close().await.unwrap();
    path
}

async fn edit(path: &Path, sql: &str) {
    let mut conn = connect(path, false).await;
    sqlx::raw_sql(sql).execute(&mut conn).await.unwrap();
    conn.close().await.unwrap();
}

async fn seed(e: &bookmole::Engine, title: &str, author: &str) -> i64 {
    e.save_book(&Book {
        title: Some(title.into()),
        authors: vec![author.into()],
        ..Default::default()
    })
    .await
    .unwrap()
    .id
    .unwrap()
}

fn only_book(r: &AppleBooksReport) -> &bookmole::AppleBooksBook {
    assert_eq!(r.books.len(), 1, "one asset with something to import");
    &r.books[0]
}

const LE_GUIN: &str = "Ursula K. Le Guin";
const LEFT_HAND: &str = "The Left Hand of Darkness";

#[tokio::test]
async fn a_store_bought_book_matched_by_title_and_author_gets_an_app_copy() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed(&e, LEFT_HAND, LE_GUIN).await;
    let root = tmp.path().join("apple");
    write_library(
        &root,
        &[store_asset("ASSET-1", LEFT_HAND, LE_GUIN, "1234567")],
    )
    .await;
    let mut note_only = hl("U-NOTE", "ASSET-1", "");
    note_only.text = None;
    note_only.note = Some("a thought of my own".into());
    let mut bookmark = hl("U-MARK", "ASSET-1", "");
    bookmark.text = None;
    bookmark.kind = 1;
    let mut gone = hl("U-GONE", "ASSET-1", "deleted before we ever saw it");
    gone.deleted = 1;
    write_annotations(
        &root,
        &[
            hl("U-HL", "ASSET-1", "Light is the left hand of darkness"),
            note_only,
            bookmark,
            gone,
        ],
    )
    .await;

    let r = import_apple_books(s, &root, false).await.unwrap();
    let b = only_book(&r);
    assert_eq!(
        b.outcome,
        AssetOutcome::Matched {
            book_id: book,
            by: AssetMatch::Title
        }
    );
    assert!(b.store_bought);
    assert_eq!((b.summary.inserted, b.summary.removed), (2, 0));
    assert_eq!((b.highlights_in_source, b.deleted_in_source), (2, 1));
    assert_eq!(r.not_highlights, 1, "the bookmark is not a highlight");

    let copies = s.copies_for_book(book).await.unwrap();
    assert_eq!(copies.len(), 1);
    let c = &copies[0];
    assert_eq!(c.form, Some(CopyForm::App));
    assert_eq!(c.app.as_deref(), Some("apple_books"));
    assert_eq!(c.tenure, Tenure::Own);
    assert_eq!(c.learned_from, LearnedFrom::AppleBooks);
    assert_eq!(
        c.have_now, None,
        "only the reader says whether they have it"
    );
    assert_eq!(c.places.len(), 1);
    assert_eq!(
        (c.places[0].kind, c.places[0].reference.as_str()),
        (PlaceKind::AppleBooks, "ASSET-1")
    );
    assert_eq!(b.copy_id, Some(c.id));

    assert_eq!(
        s.book_for_external_id(STORE_ID_SOURCE, "1234567")
            .await
            .unwrap(),
        Some(book)
    );

    let rows = s
        .imported_highlights(book, HighlightOrigin::AppleBooks)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    let h = rows
        .iter()
        .find(|h| h.origin_id.as_deref() == Some("U-HL"))
        .unwrap();
    assert_eq!(h.text, "Light is the left hand of darkness");
    assert_eq!(h.pos_kind, Some(PosKind::EpubCfi));
    assert_eq!(h.page, None, "the source gives no page");
    assert_eq!(h.ko_datetime.as_deref(), Some(WHEN_TEXT));
    assert_eq!(h.copy_id, Some(c.id));
    assert!(!h.text_unavailable);
    let n = rows
        .iter()
        .find(|h| h.origin_id.as_deref() == Some("U-NOTE"))
        .unwrap();
    assert_eq!(n.text, "", "a note-only row");
    assert!(!n.text_unavailable);
    assert_eq!(n.origin_note.as_deref(), Some("a thought of my own"));
}

#[tokio::test]
async fn a_second_import_is_all_unchanged_and_an_edited_note_updates_only_theirs() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed(&e, LEFT_HAND, LE_GUIN).await;
    let root = tmp.path().join("apple");
    write_library(
        &root,
        &[store_asset("ASSET-1", LEFT_HAND, LE_GUIN, "1234567")],
    )
    .await;
    let mut first = hl("U-1", "ASSET-1", "one");
    first.note = Some("their note".into());
    let db = write_annotations(&root, &[first, hl("U-2", "ASSET-1", "two")]).await;

    import_apple_books(s, &root, false).await.unwrap();
    let rows = s
        .imported_highlights(book, HighlightOrigin::AppleBooks)
        .await
        .unwrap();
    let id = rows
        .iter()
        .find(|h| h.origin_id.as_deref() == Some("U-1"))
        .unwrap()
        .id;
    e.set_annotation(id, Some("mine")).await.unwrap();

    let again = import_apple_books(s, &root, false).await.unwrap();
    let b = only_book(&again);
    assert_eq!(
        b.outcome,
        AssetOutcome::Matched {
            book_id: book,
            by: AssetMatch::Place
        },
        "the second time, the place decides"
    );
    assert_eq!(
        (b.summary.inserted, b.summary.updated, b.summary.unchanged),
        (0, 0, 2)
    );
    assert_eq!(s.copies_for_book(book).await.unwrap().len(), 1);

    edit(
        &db,
        "UPDATE ZAEANNOTATION SET ZANNOTATIONNOTE = 'their note, revised'
          WHERE ZANNOTATIONUUID = 'U-1'",
    )
    .await;
    let third = import_apple_books(s, &root, false).await.unwrap();
    let b = only_book(&third);
    assert_eq!((b.summary.updated, b.summary.unchanged), (1, 1));
    let h = s
        .imported_highlights(book, HighlightOrigin::AppleBooks)
        .await
        .unwrap()
        .into_iter()
        .find(|h| h.id == id)
        .unwrap();
    assert_eq!(h.origin_note.as_deref(), Some("their note, revised"));
    assert_eq!(
        h.annotation.as_deref(),
        Some("mine"),
        "ours is never touched"
    );
}

#[tokio::test]
async fn deletions_follow_the_source_keep_ours_and_ignore_absence() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed(&e, LEFT_HAND, LE_GUIN).await;
    let root = tmp.path().join("apple");
    write_library(
        &root,
        &[store_asset("ASSET-1", LEFT_HAND, LE_GUIN, "1234567")],
    )
    .await;
    let db = write_annotations(
        &root,
        &[
            hl("U-1", "ASSET-1", "goes"),
            hl("U-2", "ASSET-1", "kept, it carries mine"),
            hl("U-3", "ASSET-1", "vanishes from the database"),
            hl("U-4", "ASSET-1", "stays"),
        ],
    )
    .await;
    import_apple_books(s, &root, false).await.unwrap();
    let id_of = |rows: &[bookmole::Highlight], uuid: &str| {
        rows.iter()
            .find(|h| h.origin_id.as_deref() == Some(uuid))
            .map(|h| h.id)
    };
    let rows = s
        .imported_highlights(book, HighlightOrigin::AppleBooks)
        .await
        .unwrap();
    e.set_annotation(id_of(&rows, "U-2").unwrap(), Some("mine"))
        .await
        .unwrap();

    edit(
        &db,
        "UPDATE ZAEANNOTATION SET ZANNOTATIONDELETED = 1 WHERE ZANNOTATIONUUID IN ('U-1', 'U-2');
         DELETE FROM ZAEANNOTATION WHERE ZANNOTATIONUUID = 'U-3';",
    )
    .await;
    let r = import_apple_books(s, &root, false).await.unwrap();
    let b = only_book(&r);
    assert_eq!(b.summary.removed, 1);
    assert!(b.summary.diagnostics.iter().any(|d| matches!(
        &d.kind,
        DiagnosticKind::AppleBooksHighlightsKept { asset_id, count: 1 } if asset_id == "ASSET-1"
    )));
    let rows = s
        .imported_highlights(book, HighlightOrigin::AppleBooks)
        .await
        .unwrap();
    assert_eq!(id_of(&rows, "U-1"), None, "marked deleted: removed");
    assert!(id_of(&rows, "U-2").is_some(), "deleted but ours: kept");
    assert!(
        id_of(&rows, "U-3").is_some(),
        "absent is not a deletion mark"
    );
    assert!(id_of(&rows, "U-4").is_some());
}

#[tokio::test]
async fn a_side_loaded_epub_is_matched_through_the_ladder_and_is_the_same_copy() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let mine = tmp.path().join("library/Gethenian Tales.epub");
    std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
    write_isbnless_epub_by(&mine, "Gethenian Tales", LE_GUIN);
    let imported = e
        .import_file(&mine, FileImportOptions::default())
        .await
        .unwrap();
    let book = imported.book_id.unwrap();
    // Same bytes under a title Apple made up: the title rung cannot find it, only the file can.
    let theirs = tmp.path().join("container/Books/0A1B2C.epub");
    std::fs::create_dir_all(theirs.parent().unwrap()).unwrap();
    std::fs::copy(&mine, &theirs).unwrap();
    let root = tmp.path().join("apple");
    write_library(
        &root,
        &[Asset {
            id: "SIDE-1".into(),
            title: Some("0A1B2C".into()),
            author: None,
            path: Some(theirs.display().to_string()),
            store_id: None,
        }],
    )
    .await;
    write_annotations(&root, &[hl("U-1", "SIDE-1", "words")]).await;

    let r = import_apple_books(s, &root, false).await.unwrap();
    let b = only_book(&r);
    assert_eq!(
        b.outcome,
        AssetOutcome::Matched {
            book_id: book,
            by: AssetMatch::File(FileMatch::Sha256)
        }
    );
    assert!(!b.store_bought);
    let copies = s.copies_for_book(book).await.unwrap();
    assert_eq!(copies.len(), 1, "one file is one copy wherever it sits");
    let kinds: Vec<PlaceKind> = copies[0].places.iter().map(|p| p.kind).collect();
    assert!(kinds.contains(&PlaceKind::Path) && kinds.contains(&PlaceKind::AppleBooks));
    assert_eq!(copies[0].form, Some(CopyForm::File));
    assert_eq!(b.copy_id, Some(copies[0].id));
    assert!(s.book_files(book).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_unknown_side_loaded_file_with_highlights_becomes_a_book_with_a_file_copy() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let theirs = tmp.path().join("container/Books/orphan.epub");
    std::fs::create_dir_all(theirs.parent().unwrap()).unwrap();
    write_isbnless_epub_by(&theirs, "The Word for World Is Forest", LE_GUIN);
    let root = tmp.path().join("apple");
    write_library(
        &root,
        &[Asset {
            id: "SIDE-2".into(),
            title: Some("The Word for World Is Forest".into()),
            author: Some(LE_GUIN.into()),
            path: Some(theirs.display().to_string()),
            store_id: None,
        }],
    )
    .await;
    write_annotations(&root, &[hl("U-1", "SIDE-2", "words")]).await;

    let r = import_apple_books(s, &root, false).await.unwrap();
    let AssetOutcome::Created { book_id } = only_book(&r).outcome else {
        panic!("created: {:?}", only_book(&r).outcome);
    };
    let book = s.get_book(book_id).await.unwrap().unwrap();
    assert_eq!(book.title.as_deref(), Some("The Word for World Is Forest"));
    let copies = s.copies_for_book(book_id).await.unwrap();
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].form, Some(CopyForm::File));
    assert_eq!(copies[0].learned_from, LearnedFrom::AppleBooks);
    let place = &copies[0].places[0];
    assert_eq!(place.kind, PlaceKind::AppleBooks);
    assert_eq!(
        place.last_hash.as_deref(),
        Some(bookmole::files::sha256_of(&theirs).unwrap().as_str())
    );

    let again = import_apple_books(s, &root, false).await.unwrap();
    assert!(matches!(
        only_book(&again).outcome,
        AssetOutcome::Matched {
            by: AssetMatch::Place,
            ..
        }
    ));
    assert_eq!(only_book(&again).summary.unchanged, 1);
}

#[tokio::test]
async fn assets_without_highlights_are_skipped_and_counted_never_created() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let root = tmp.path().join("apple");
    write_library(
        &root,
        &[
            store_asset("ASSET-A", "A Wizard of Earthsea", LE_GUIN, "1"),
            store_asset("ASSET-B", "The Tombs of Atuan", LE_GUIN, "2"),
            store_asset("ASSET-C", "The Farthest Shore", LE_GUIN, "3"),
        ],
    )
    .await;
    let mut bookmark = hl("U-MARK", "ASSET-B", "");
    bookmark.kind = 1;
    let mut deleted = hl("U-DEL", "ASSET-C", "deleted");
    deleted.deleted = 1;
    write_annotations(&root, &[bookmark, deleted]).await;

    let r = import_apple_books(s, &root, false).await.unwrap();
    assert!(r.books.is_empty());
    assert_eq!(r.assets_in_library, 3);
    assert_eq!(r.assets_skipped, 3);
    assert!(
        e.list_books(&Default::default()).await.unwrap().is_empty(),
        "no highlight, no book"
    );
}

#[tokio::test]
async fn a_store_bought_book_with_highlights_and_no_match_is_created() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let root = tmp.path().join("apple");
    write_library(
        &root,
        &[store_asset(
            "ASSET-A",
            "A Wizard of Earthsea",
            LE_GUIN,
            "99",
        )],
    )
    .await;
    write_annotations(&root, &[hl("U-1", "ASSET-A", "To light a candle")]).await;
    let r = import_apple_books(s, &root, false).await.unwrap();
    let AssetOutcome::Created { book_id } = only_book(&r).outcome else {
        panic!("created");
    };
    let book = s.get_book(book_id).await.unwrap().unwrap();
    assert_eq!(book.title.as_deref(), Some("A Wizard of Earthsea"));
    assert_eq!(book.authors, vec![LE_GUIN.to_string()]);
    let copies = s.copies_for_book(book_id).await.unwrap();
    assert_eq!(copies[0].form, Some(CopyForm::App));
    assert_eq!(
        s.book_for_external_id(STORE_ID_SOURCE, "99").await.unwrap(),
        Some(book_id)
    );
}

#[tokio::test]
async fn a_near_miss_is_refused_with_candidates_and_a_link_resolves_it() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let near = seed(
        &e,
        "Middlemarch: A Study of Provincial Life",
        "Eliot, George",
    )
    .await;
    let root = tmp.path().join("apple");
    write_library(
        &root,
        &[store_asset("ASSET-P", "Middlemarch", "George Eliot", "7")],
    )
    .await;
    write_annotations(
        &root,
        &[hl(
            "U-1",
            "ASSET-P",
            "We are all of us born in moral stupidity",
        )],
    )
    .await;

    let r = import_apple_books(s, &root, false).await.unwrap();
    let AssetOutcome::Unmatched { candidates } = &only_book(&r).outcome else {
        panic!("unmatched: {:?}", only_book(&r).outcome);
    };
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].book_id, near);
    assert_eq!(only_book(&r).highlights_in_source, 1);
    assert_eq!(e.list_books(&Default::default()).await.unwrap().len(), 1);
    assert!(s.copies_for_book(near).await.unwrap().is_empty());

    let copy = link_apple_books_asset(s, "ASSET-P", near).await.unwrap();
    let again = import_apple_books(s, &root, false).await.unwrap();
    assert_eq!(
        only_book(&again).outcome,
        AssetOutcome::Matched {
            book_id: near,
            by: AssetMatch::Place
        }
    );
    assert_eq!(only_book(&again).copy_id, Some(copy));
    assert_eq!(only_book(&again).summary.inserted, 1);
}

/// The writer stays open across the import (a clean close would checkpoint away the WAL), and
/// the originals' bytes and mtimes are compared before and after.
#[tokio::test]
async fn the_wal_is_copied_and_read_and_the_originals_are_never_written() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed(&e, LEFT_HAND, LE_GUIN).await;
    let root = tmp.path().join("apple");
    let lib = write_library(&root, &[store_asset("ASSET-1", LEFT_HAND, LE_GUIN, "1")]).await;
    let db = annotation_path(&root, "v10312011_1727");
    let mut writer = connect(&db, true).await;
    sqlx::raw_sql(ANNOTATION_DDL)
        .execute(&mut writer)
        .await
        .unwrap();
    insert_annotations(
        &mut writer,
        &[hl("U-WAL", "ASSET-1", "only in the write-ahead log")],
    )
    .await;

    let sibling = |p: &Path, s: &str| {
        let mut n = p.as_os_str().to_os_string();
        n.push(s);
        PathBuf::from(n)
    };
    let wal = sibling(&db, "-wal");
    assert!(
        wal.is_file() && std::fs::metadata(&wal).unwrap().len() > 0,
        "the test needs a real -wal"
    );
    let main = std::fs::read(&db).unwrap();
    assert!(
        !main
            .windows(b"only in the write-ahead".len())
            .any(|w| w == b"only in the write-ahead"),
        "the row must not have been checkpointed into the main file"
    );

    let files = [db.clone(), wal.clone(), sibling(&db, "-shm"), lib.clone()];
    let before: Vec<Option<(Vec<u8>, SystemTime)>> = files
        .iter()
        .map(|f| {
            std::fs::read(f)
                .ok()
                .map(|b| (b, std::fs::metadata(f).unwrap().modified().unwrap()))
        })
        .collect();

    let r = import_apple_books(s, &root, false).await.unwrap();
    assert_eq!(r.annotation_db.as_deref(), Some(db.as_path()));
    assert_eq!(only_book(&r).summary.inserted, 1);
    let rows = s
        .imported_highlights(book, HighlightOrigin::AppleBooks)
        .await
        .unwrap();
    assert_eq!(rows[0].text, "only in the write-ahead log");

    let after: Vec<Option<(Vec<u8>, SystemTime)>> = files
        .iter()
        .map(|f| {
            std::fs::read(f)
                .ok()
                .map(|b| (b, std::fs::metadata(f).unwrap().modified().unwrap()))
        })
        .collect();
    for (f, (b, a)) in files.iter().zip(before.iter().zip(&after)) {
        assert!(b == a, "{} changed under the import", f.display());
    }
    assert!(before[2].is_some(), "the -shm existed and was checked too");
    writer.close().await.unwrap();
}

#[tokio::test]
async fn of_two_databases_with_different_suffixes_the_newest_is_read() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed(&e, LEFT_HAND, LE_GUIN).await;
    let root = tmp.path().join("apple");
    write_library(&root, &[store_asset("ASSET-1", LEFT_HAND, LE_GUIN, "1")]).await;
    let old = write_annotations_named(&root, "v1_0001", &[hl("U-OLD", "ASSET-1", "old")]).await;
    let new = write_annotations_named(&root, "v2_0002", &[hl("U-NEW", "ASSET-1", "new")]).await;
    let set = |p: &Path, t: SystemTime| {
        std::fs::OpenOptions::new()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap()
    };
    let now = SystemTime::now();
    // The lexically later name is the older file, so name order cannot pass.
    set(&new, now - Duration::from_secs(3600));
    set(&old, now);

    let r = import_apple_books(s, &root, false).await.unwrap();
    assert_eq!(r.annotation_db.as_deref(), Some(old.as_path()));
    let rows = s
        .imported_highlights(book, HighlightOrigin::AppleBooks)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].origin_id.as_deref(), Some("U-OLD"));
}

#[tokio::test]
async fn no_databases_at_all_is_a_diagnostic_not_an_error() {
    let (tmp, e) = engine().await;
    let r = import_apple_books(e.storage(), &tmp.path().join("nothing-here"), false)
        .await
        .unwrap();
    assert!(r.books.is_empty());
    assert_eq!(r.annotation_db, None);
    let absent: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| matches!(d.kind, DiagnosticKind::AppleBooksAbsent { .. }))
        .collect();
    assert_eq!(absent.len(), 2, "{:?}", r.diagnostics);
    assert!(
        absent[0]
            .to_string()
            .ends_with("Apple Books has no data here")
    );

    let root = tmp.path().join("apple");
    std::fs::create_dir_all(root.join(ANNOTATION_DIR)).unwrap();
    std::fs::write(root.join(ANNOTATION_DIR).join("notes.txt"), b"x").unwrap();
    let r = import_apple_books(e.storage(), &root, false).await.unwrap();
    assert!(
        r.diagnostics
            .iter()
            .any(|d| matches!(&d.kind, DiagnosticKind::AppleBooksAbsent { path } if path.ends_with(ANNOTATION_DIR)))
    );
}

#[tokio::test]
async fn a_missing_optional_column_degrades_and_a_missing_table_is_named() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed(&e, LEFT_HAND, LE_GUIN).await;
    let root = tmp.path().join("apple");
    write_library(&root, &[store_asset("ASSET-1", LEFT_HAND, LE_GUIN, "1")]).await;
    let db = annotation_path(&root, "v0_old");
    let mut conn = connect(&db, false).await;
    sqlx::raw_sql(ANNOTATION_DDL_OLD)
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO ZAEANNOTATION (ZANNOTATIONUUID, ZANNOTATIONASSETID, ZANNOTATIONSELECTEDTEXT,
             ZANNOTATIONDELETED, ZANNOTATIONCREATIONDATE, ZANNOTATIONLOCATION)
         VALUES ('U-1', 'ASSET-1', 'old schema words', 0, 'not a date', 'epubcfi(/6/2!/4)')",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    conn.close().await.unwrap();

    let r = import_apple_books(s, &root, false).await.unwrap();
    let missing = r
        .diagnostics
        .iter()
        .find_map(|d| match &d.kind {
            DiagnosticKind::AppleBooksColumnsMissing { table, columns, .. } => {
                Some((table.clone(), columns.clone()))
            }
            _ => None,
        })
        .expect("the missing columns are reported");
    assert_eq!(missing.0, "ZAEANNOTATION");
    assert!(missing.1.contains(&"ZANNOTATIONSTYLE".to_string()));
    assert!(missing.1.contains(&"ZANNOTATIONTYPE".to_string()));
    assert_eq!(only_book(&r).summary.inserted, 1);
    let rows = s
        .imported_highlights(book, HighlightOrigin::AppleBooks)
        .await
        .unwrap();
    assert_eq!(rows[0].text, "old schema words");
    assert_eq!(
        rows[0].ko_datetime, None,
        "a text date is no date, not 2001"
    );

    let root2 = tmp.path().join("apple2");
    let lib = library_path(&root2, "x");
    let mut conn = connect(&lib, false).await;
    sqlx::raw_sql("CREATE TABLE ZSOMETHINGELSE (Z_PK INTEGER PRIMARY KEY)")
        .execute(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();
    write_annotations(&root2, &[hl("U-9", "ASSET-9", "nowhere to go")]).await;
    let r = import_apple_books(s, &root2, false).await.unwrap();
    assert!(r.diagnostics.iter().any(|d| matches!(
        &d.kind,
        DiagnosticKind::AppleBooksSchemaUnknown { table, .. } if table == "ZBKLIBRARYASSET"
    )));
    assert!(r.books.is_empty());
    assert_eq!(r.orphaned, 1);
}

#[tokio::test]
async fn a_dry_run_writes_nothing() {
    let (tmp, e) = engine().await;
    let s = e.storage();
    let book = seed(&e, LEFT_HAND, LE_GUIN).await;
    let root = tmp.path().join("apple");
    write_library(
        &root,
        &[
            store_asset("ASSET-1", LEFT_HAND, LE_GUIN, "1"),
            store_asset("ASSET-2", "A Wizard of Earthsea", LE_GUIN, "2"),
        ],
    )
    .await;
    write_annotations(
        &root,
        &[hl("U-1", "ASSET-1", "one"), hl("U-2", "ASSET-2", "two")],
    )
    .await;

    let r = import_apple_books(s, &root, true).await.unwrap();
    assert!(r.dry_run);
    assert_eq!(r.books.len(), 2);
    let outcome = |id: &str| {
        r.books
            .iter()
            .find(|b| b.asset_id == id)
            .unwrap()
            .outcome
            .clone()
    };
    assert_eq!(
        outcome("ASSET-1"),
        AssetOutcome::Matched {
            book_id: book,
            by: AssetMatch::Title
        }
    );
    assert_eq!(outcome("ASSET-2"), AssetOutcome::WouldCreate);
    assert_eq!(r.totals.inserted, 0);
    assert!(r.books.iter().all(|b| b.highlights_in_source == 1));

    assert_eq!(e.list_books(&Default::default()).await.unwrap().len(), 1);
    assert!(s.copies_for_book(book).await.unwrap().is_empty());
    assert!(
        s.imported_highlights(book, HighlightOrigin::AppleBooks)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s.book_for_external_id(STORE_ID_SOURCE, "1").await.unwrap(),
        None
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_folder_we_may_not_read_is_its_own_diagnostic() {
    use std::os::unix::fs::PermissionsExt;
    let (tmp, e) = engine().await;
    let root = tmp.path().join("apple");
    write_annotations(&root, &[hl("U-1", "ASSET-1", "x")]).await;
    let dir = root.join(ANNOTATION_DIR);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&dir).is_ok() {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        common::skipped(
            "permissions are not enforced for this user (root?)",
            "run the suite as an ordinary user",
        );
        return;
    }
    let r = import_apple_books(e.storage(), &root, false).await.unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(r.diagnostics.iter().any(|d| matches!(
        &d.kind,
        DiagnosticKind::AppleBooksPermissionDenied { path } if path == &dir
    )));
    assert!(r.books.is_empty());
}

#[tokio::test]
async fn the_facade_reads_the_root_it_was_given_and_says_absent_without_one() {
    let (tmp, e) = engine().await;
    let root = tmp.path().join("apple");
    let e = e.with_apple_books_root(Some(root.clone()));
    let near = seed(
        &e,
        "Middlemarch: A Study of Provincial Life",
        "Eliot, George",
    )
    .await;
    let left = seed(&e, LEFT_HAND, LE_GUIN).await;
    write_library(
        &root,
        &[
            store_asset("ASSET-1", LEFT_HAND, LE_GUIN, "1234567"),
            store_asset("ASSET-P", "Middlemarch", "George Eliot", "7"),
        ],
    )
    .await;
    write_annotations(
        &root,
        &[
            hl("U-HL", "ASSET-1", "Light is the left hand of darkness"),
            hl("U-P", "ASSET-P", "We are all of us born in moral stupidity"),
        ],
    )
    .await;

    let dry = e.import_apple_books(true).await.unwrap();
    assert!(dry.dry_run);
    assert_eq!(dry.books.len(), 2);
    assert!(e.copies_for_book(left).await.unwrap().is_empty());

    let r = e.import_apple_books(false).await.unwrap();
    let by_asset = |r: &AppleBooksReport, id: &str| {
        r.books.iter().find(|b| b.asset_id == id).cloned().unwrap()
    };
    assert_eq!(
        by_asset(&r, "ASSET-1").outcome,
        AssetOutcome::Matched {
            book_id: left,
            by: AssetMatch::Title
        }
    );
    assert!(matches!(
        by_asset(&r, "ASSET-P").outcome,
        AssetOutcome::Unmatched { .. }
    ));

    let explicit = e.import_apple_books_from(&root, false).await.unwrap();
    assert_eq!(by_asset(&explicit, "ASSET-1").summary.unchanged, 1);

    let copy = e.link_apple_books_asset("ASSET-P", near).await.unwrap();
    assert_eq!(
        e.link_apple_books_asset("ASSET-P", near).await.unwrap(),
        copy
    );
    let again = e.import_apple_books(false).await.unwrap();
    assert_eq!(by_asset(&again, "ASSET-P").copy_id, Some(copy));
    assert_eq!(by_asset(&again, "ASSET-P").summary.inserted, 1);

    let e = e.with_apple_books_root(None);
    let none = e.import_apple_books(false).await.unwrap();
    assert!(none.books.is_empty());
    assert_eq!(none.annotation_db, None);
    assert!(
        none.diagnostics
            .iter()
            .any(|d| matches!(d.kind, DiagnosticKind::AppleBooksAbsent { .. })),
        "{:?}",
        none.diagnostics
    );
}
