//! Real engines, each with its own data directory and database file, syncing through
//! [`TestServer`] with [`Engine::sync_now`]'s own machinery.

use std::path::PathBuf;

use sqlx::{ConnectOptions, Connection};

use super::Receiver;
use super::http::Origin;
use super::test_server::TestServer;
use crate::config::EngineConfig;
use crate::error::EngineError;
use crate::lists::{EntryShow, EntrySort};
use crate::storage::{NewHighlight, NewNoteMeta, SearchSource};
use crate::{Book, Engine, RecoveryCode, SyncError, SyncOutcome, SyncStatus};

struct Device {
    _dir: tempfile::TempDir,
    root: PathBuf,
    engine: Engine,
}

async fn device() -> Device {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let engine = Engine::open(EngineConfig::rooted_at(&root)).await.unwrap();
    Device {
        _dir: dir,
        root,
        engine,
    }
}

fn origin() -> Origin {
    Origin::parse("https://sync.example.test").unwrap()
}

async fn create(d: &Device, server: &TestServer) -> RecoveryCode {
    d.engine
        .create_sync_library_via(origin(), "Study", server)
        .await
        .unwrap()
}

async fn join(d: &Device, server: &TestServer, code: &RecoveryCode) {
    d.engine
        .join_sync_library_via(origin(), code.as_str(), "Reading chair", server)
        .await
        .unwrap();
}

async fn sync_with(
    d: &Device,
    server: &TestServer,
    receiver: Receiver,
) -> crate::Result<SyncOutcome> {
    d.engine
        .sync_now_with(|_| Ok(server.clone()), receiver)
        .await
}

async fn sync(d: &Device, server: &TestServer) -> SyncOutcome {
    sync_with(d, server, Receiver::default()).await.unwrap()
}

/// Every device syncs in turn until a whole round moves nothing; what is held is then empty.
async fn converge(devices: &[&Device], server: &TestServer) {
    for _ in 0..6 {
        let mut moved = false;
        for d in devices {
            let got = sync(d, server).await;
            assert!(got.held.is_empty(), "held: {:?}", got.held);
            moved |= got.received || got.sent;
        }
        if !moved {
            return;
        }
    }
    panic!("the devices kept sending changes");
}

async fn scalar<T>(d: &Device, sql: &str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite> + Send + Unpin,
{
    sqlx::query_scalar(sql)
        .fetch_one(d.engine.storage().pool())
        .await
        .unwrap()
}

async fn rows(d: &Device, sql: &str) -> Vec<String> {
    sqlx::query_scalar(sql)
        .fetch_all(d.engine.storage().pool())
        .await
        .unwrap()
}

async fn book(d: &Device, title: &str) -> i64 {
    sqlx::query_scalar("SELECT id FROM books WHERE title = ?")
        .bind(title)
        .fetch_one(d.engine.storage().pool())
        .await
        .unwrap()
}

async fn add_book(d: &Device, title: &str) -> i64 {
    let book = Book {
        title: Some(title.into()),
        authors: vec!["George Eliot".into()],
        ..Default::default()
    };
    d.engine.storage().upsert_book(&book, None).await.unwrap()
}

const PASSAGE: &str = "Who can quit young lives after being long in company with them?";

async fn highlight(d: &Device, book: i64) {
    d.engine
        .storage()
        .insert_highlight(
            book,
            &NewHighlight {
                text: PASSAGE.into(),
                chapter: Some("Finale".into()),
                page: Some(838),
                pos0: None,
                pos1: None,
                ko_datetime: None,
                ko_datetime_updated: None,
                color: None,
                note: None,
                source: "koreader".into(),
            },
        )
        .await
        .unwrap()
        .unwrap();
}

/// A cover file `d` holds for `book`, as a download would have left it.
async fn cover_file(d: &Device, book: i64) -> PathBuf {
    cover_file_from(d, book, crate::images::CoverOrigin::File).await
}

/// A cover file `d` holds for `book`, as storing it from a file or a download would have left
/// it, and old enough for a sweep.
async fn cover_file_from(d: &Device, book: i64, origin: crate::images::CoverOrigin) -> PathBuf {
    let images = d.root.join("database/images");
    std::fs::create_dir_all(&images).unwrap();
    let prefix = match origin {
        crate::images::CoverOrigin::File => "",
        crate::images::CoverOrigin::Url => crate::images::DOWNLOADED,
    };
    let hash = format!("{:02x}", book as u8).repeat(32);
    let file = images.join(format!("{prefix}{hash}.jpg"));
    std::fs::write(&file, b"cover").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(old)
        .unwrap();
    sqlx::query("UPDATE books SET cover_path = ? WHERE id = ?")
        .bind(file.display().to_string())
        .bind(book)
        .execute(d.engine.storage().pool())
        .await
        .unwrap();
    file
}

async fn list_order(d: &Device) -> Vec<String> {
    let list = scalar::<i64>(d, "SELECT id FROM book_lists").await;
    d.engine
        .list_entries(list, EntryShow::All, EntrySort::Yours)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.book.title.unwrap_or_default())
        .collect()
}

const KEYS: &str =
    "SELECT title || ' ' || COALESCE(sort_title, '-') || ' ' || COALESCE(sort_author, '-')
                      FROM books ORDER BY title";

/// What every device shows once the writes of the test below have reached it.
async fn shows_what_was_written(d: &Device, keys: &[String]) {
    assert_eq!(rows(d, KEYS).await, keys);
    let found = d
        .engine
        .storage()
        .search_books(
            "middlemarch",
            &crate::storage::BookFilter::default(),
            crate::storage::LibraryScope::All,
            10,
        )
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    for (query, source) in [
        ("young lives", SearchSource::Highlight),
        ("great name", SearchSource::Note),
    ] {
        let hits = d
            .engine
            .search_marks(query, Some(source), None, 10)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "{query}");
    }
    assert_eq!(list_order(d).await, ["Romola", "Middlemarch", "Adam Bede"]);
    assert_eq!(
        rows(d, "SELECT tag || ' ' || source FROM book_tags").await,
        ["to-read goodreads"]
    );
    assert_eq!(
        rows(
            d,
            "SELECT n.kind || ' ' || r.value FROM review_ratings r JOIN notes n ON n.id = r.note_id"
        )
        .await,
        ["review 4.0"]
    );
}

#[tokio::test]
async fn three_engines_share_what_each_writes_and_derive_the_same_from_it() {
    let server = TestServer::default();
    let (a, b, c) = (device().await, device().await, device().await);
    let code = create(&a, &server).await;
    join(&b, &server, &code).await;
    join(&c, &server, &code).await;
    let all = [&a, &b, &c];

    let middlemarch = add_book(&a, "Middlemarch").await;
    add_book(&a, "Adam Bede").await;
    add_book(&a, "Romola").await;
    add_book(&a, "Felix Holt").await;
    highlight(&a, middlemarch).await;
    converge(&all, &server).await;

    let on_b = book(&b, "Middlemarch").await;
    let review = b
        .engine
        .storage()
        .insert_note(
            NewNoteMeta {
                book_id: Some(on_b),
                reading_id: None,
                highlight_id: None,
                page: None,
                location: None,
                file_path: "Middlemarch/review.md",
                title: "Middlemarch",
                kind: "review",
            },
            "Her full nature spent itself in channels which had no great name on the earth.",
            &[],
        )
        .await
        .unwrap();
    let scale = b
        .engine
        .storage()
        .rating_scale_by_name("default")
        .await
        .unwrap()
        .unwrap();
    b.engine
        .storage()
        .set_review_rating(review, &scale, 4.0)
        .await
        .unwrap();
    let list = b.engine.create_list("Eliot", None).await.unwrap().id;
    for title in ["Middlemarch", "Adam Bede", "Romola"] {
        let id = book(&b, title).await;
        b.engine.add_to_list(list, id, None).await.unwrap();
    }
    converge(&all, &server).await;

    let on_c = book(&c, "Middlemarch").await;
    sqlx::query("INSERT INTO book_tags (book_id, tag, source) VALUES (?, 'to-read', 'goodreads')")
        .bind(on_c)
        .execute(c.engine.storage().pool())
        .await
        .unwrap();
    let list_c = scalar::<i64>(&c, "SELECT id FROM book_lists").await;
    let romola = book(&c, "Romola").await;
    c.engine.move_in_list(list_c, romola, 0).await.unwrap();
    let felix: Vec<PathBuf> = {
        let mut files = Vec::new();
        for d in [&b, &c] {
            files.push(cover_file(d, book(d, "Felix Holt").await).await);
        }
        files
    };
    a.engine
        .delete_book(book(&a, "Felix Holt").await)
        .await
        .unwrap();
    converge(&all, &server).await;

    let keys =
        "SELECT title || ' ' || COALESCE(sort_title, '-') || ' ' || COALESCE(sort_author, '-')
                  FROM books ORDER BY title";
    let made_here = rows(&a, keys).await;
    assert_eq!(made_here.len(), 3, "{made_here:?}");
    for d in all {
        shows_what_was_written(d, &made_here).await;
    }
    for file in &felix {
        assert!(!file.exists(), "{}", file.display());
    }

    for d in all {
        let again = sync(d, &server).await;
        assert!(!again.received && !again.sent, "{again:?}");
    }
}

#[tokio::test]
async fn two_devices_opening_a_reading_of_one_book_settle_on_one_open_everywhere() {
    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    let code = create(&a, &server).await;
    join(&b, &server, &code).await;
    add_book(&a, "Middlemarch").await;
    converge(&[&a, &b], &server).await;

    for d in [&a, &b] {
        let id = book(d, "Middlemarch").await;
        d.engine
            .storage()
            .open_reading(id, Some(100), "manual")
            .await
            .unwrap();
    }
    sync(&a, &server).await;
    // When a's reading is the one that loses, b holds it until a's next sync gives it up.
    let first = sync(&b, &server).await;
    assert!(
        first.held.iter().all(|h| h.collection == "readings"),
        "{:?}",
        first.held
    );
    converge(&[&a, &b], &server).await;
    for d in [&a, &b] {
        let open =
            scalar::<i64>(d, "SELECT count(*) FROM readings WHERE finished_at IS NULL").await;
        assert_eq!(open, 1);
        assert_eq!(scalar::<i64>(d, "SELECT count(*) FROM readings").await, 2);
    }
}

#[tokio::test]
async fn two_syncs_at_once_take_turns_and_both_succeed() {
    let server = TestServer::default();
    let a = device().await;
    create(&a, &server).await;
    add_book(&a, "Middlemarch").await;
    let before = server.endpoints().len();
    server.hold_at(2);
    let second = async {
        server.held().await;
        let arrived = server.arrived();
        let second = sync(&a, &server);
        tokio::pin!(second);
        let raced = tokio::time::timeout(std::time::Duration::from_millis(300), &mut second).await;
        assert!(
            raced.is_err(),
            "the second sync ran while the first was in flight"
        );
        assert_eq!(
            server.arrived(),
            arrived,
            "the second sync reached the server"
        );
        server.release();
        second.await
    };
    let (first, second) = tokio::join!(sync(&a, &server), second);
    assert!(first.sent && !second.sent, "{first:?} {second:?}");
    assert!(!second.received);
    let seen = &server.endpoints()[before..];
    assert_eq!(
        seen.iter().filter(|e| **e == "append").count(),
        1,
        "{seen:?}"
    );
}

#[tokio::test]
async fn a_sync_while_another_connection_writes_waits_its_turn_and_loses_nothing() {
    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    let code = create(&a, &server).await;
    join(&b, &server, &code).await;
    converge(&[&a, &b], &server).await;
    add_book(&a, "Middlemarch").await;
    sync(&a, &server).await;

    let file = b.root.join("database/app.db");
    let mut writer = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&file)
        .connect()
        .await
        .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut writer)
        .await
        .unwrap();
    sqlx::query("INSERT INTO books (title, created_at, last_modified) VALUES ('Romola', 1, 1)")
        .execute(&mut writer)
        .await
        .unwrap();
    let release = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        sqlx::query("COMMIT").execute(&mut writer).await.unwrap();
        writer.close().await.unwrap();
    });
    let during = sync_with(&b, &server, Receiver::default()).await;
    release.await.unwrap();
    match during {
        Ok(_) | Err(EngineError::Sync(SyncError::Interrupted)) => {}
        Err(other) => panic!("{other:?}"),
    }
    assert!(b.engine.sync.held.lock().await.busy_retries > 0);

    converge(&[&a, &b], &server).await;
    for d in [&a, &b] {
        assert_eq!(
            rows(d, "SELECT title FROM books ORDER BY title").await,
            ["Middlemarch", "Romola"]
        );
        assert_eq!(rows(d, "PRAGMA integrity_check").await, ["ok"]);
    }
}

#[tokio::test]
async fn a_pull_that_fails_part_way_keeps_nothing_it_did_not_commit_and_the_next_recovers() {
    let server = TestServer::default();
    let (a, b, c) = (device().await, device().await, device().await);
    let code = create(&a, &server).await;
    join(&b, &server, &code).await;
    join(&c, &server, &code).await;
    let middlemarch = add_book(&a, "Middlemarch").await;
    add_book(&a, "Romola").await;
    converge(&[&a, &b, &c], &server).await;
    let mut files = Vec::new();
    for d in [&b, &c] {
        files.push(cover_file(d, book(d, "Romola").await).await);
    }

    let lagging = server.log_len();
    a.engine
        .delete_book(book(&a, "Romola").await)
        .await
        .unwrap();
    sync(&a, &server).await;
    highlight(&a, middlemarch).await;
    sync(&a, &server).await;
    add_book(&a, "Adam Bede").await;
    sync(&a, &server).await;
    let titles = "SELECT title FROM books ORDER BY title";

    // Every apply fails: nothing of the pull is kept, and the file stays.
    let before = b.engine.sync_status().await.unwrap();
    let mut failing = Receiver::default();
    failing.faults.fail_applies = usize::MAX;
    let failed = sync_with(&b, &server, failing).await;
    assert!(matches!(failed, Err(EngineError::Other(_))), "{failed:?}");
    assert_eq!(rows(&b, titles).await, ["Middlemarch", "Romola"]);
    assert_eq!(
        scalar::<i64>(&b, "SELECT count(*) FROM highlights").await,
        0
    );
    assert!(files[0].exists());
    assert_eq!(b.engine.sync_status().await.unwrap(), before);

    // c's own envelope lands after a's, which c has not read, so its next pull commits part-way.
    // That commit fails once; the client applies again, which commits, and still ends the sync
    // with the error, so the file waits for the next sync that succeeds.
    server.lag_reads(Some(lagging));
    add_book(&c, "Daniel Deronda").await;
    assert!(sync(&c, &server).await.sent);
    server.lag_reads(None);
    let mut once = Receiver::default();
    once.faults.fail_applies = 1;
    let failed = sync_with(&c, &server, once).await;
    assert!(matches!(failed, Err(EngineError::Other(_))), "{failed:?}");
    let all_three = ["Adam Bede", "Daniel Deronda", "Middlemarch"];
    assert_eq!(rows(&c, titles).await, all_three);
    assert!(files[1].exists());

    for d in [&b, &c] {
        sync(d, &server).await;
        assert_eq!(rows(d, titles).await, all_three);
        assert_eq!(scalar::<i64>(d, "SELECT count(*) FROM highlights").await, 1);
    }
    for file in &files {
        assert!(!file.exists(), "{}", file.display());
    }
}

/// Whether another connection can take the write lock at once, without waiting.
async fn writes_at_once(d: &Device) -> bool {
    let mut conn = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(d.root.join("database/app.db"))
        .busy_timeout(std::time::Duration::ZERO)
        .connect()
        .await
        .unwrap();
    let wrote = sqlx::raw_sql("BEGIN IMMEDIATE; COMMIT;")
        .execute(&mut conn)
        .await
        .is_ok();
    conn.close().await.unwrap();
    wrote
}

/// Whether another connection gets the write lock within a second, trying without waiting:
/// a dropped sync's connection is closed on its worker straight away, not when the next sync
/// comes, and the engine's own writes then go through.
async fn released(d: &Device) -> bool {
    for _ in 0..100 {
        if writes_at_once(d).await {
            add_book(d, "Felix Holt").await;
            sqlx::query("DELETE FROM books WHERE title = 'Felix Holt'")
                .execute(d.engine.storage().pool())
                .await
                .unwrap();
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    false
}

#[tokio::test]
async fn a_sync_dropped_or_panicking_anywhere_leaves_the_database_writable() {
    use futures::FutureExt;

    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    let code = create(&a, &server).await;
    join(&b, &server, &code).await;
    converge(&[&a, &b], &server).await;
    for title in ["Middlemarch", "Romola", "Adam Bede"] {
        add_book(&a, title).await;
        sync(&a, &server).await;
    }

    let mut hanging = Receiver::default();
    hanging.faults.hang = true;
    let dropped = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        sync_with(&b, &server, hanging),
    )
    .await;
    assert!(dropped.is_err(), "the apply did not hang");
    assert!(released(&b).await);

    let mut panicking = Receiver::default();
    panicking.faults.panic = true;
    let caught = std::panic::AssertUnwindSafe(sync_with(&b, &server, panicking))
        .catch_unwind()
        .await;
    assert!(caught.is_err(), "the apply did not panic");
    assert!(released(&b).await);

    for n in 1..=8 {
        server.hang_at(Some(n));
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            sync_with(&b, &server, Receiver::default()),
        )
        .await;
        server.hang_at(None);
        assert!(released(&b).await, "dropped at request {n}");
    }

    let opened = b.engine.sync.held.lock().await.opened;
    sync(&b, &server).await;
    assert!(b.engine.sync.held.lock().await.opened > opened);
    assert_eq!(
        rows(&b, "SELECT title FROM books ORDER BY title").await,
        ["Adam Bede", "Middlemarch", "Romola"]
    );
}

#[tokio::test]
async fn a_cover_from_a_file_here_outlasts_a_cover_url_from_elsewhere() {
    use crate::storage::Source;

    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    let code = create(&a, &server).await;
    let isbn = "9780141439549";
    let middlemarch = Book {
        title: Some("Middlemarch".into()),
        isbn_13: Some(isbn.into()),
        ..Default::default()
    };
    let on_a = a
        .engine
        .storage()
        .upsert_book(&middlemarch, None)
        .await
        .unwrap();
    a.engine
        .storage()
        .enrich_book(
            on_a,
            &Book {
                cover_url: Some("https://covers.example.test/1.jpg".into()),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();
    add_book(&a, "Romola").await;
    sync(&a, &server).await;

    // b's own copy of the book has its EPUB's cover, and another book a downloaded one.
    let on_b = b
        .engine
        .storage()
        .upsert_book(&middlemarch, None)
        .await
        .unwrap();
    let epub_cover = cover_file(&b, on_b).await;
    join(&b, &server, &code).await;
    let first = sync(&b, &server).await;
    converge(&[&a, &b], &server).await;
    let kept = book(&b, "Middlemarch").await;
    assert_eq!(
        scalar::<Option<String>>(
            &b,
            &format!("SELECT cover_path FROM books WHERE id = {kept}")
        )
        .await,
        Some(epub_cover.display().to_string())
    );
    assert!(epub_cover.exists());
    assert!(!first.covers_to_fetch.contains(&kept), "{first:?}");

    // A downloaded cover is still replaced when the URL moves elsewhere.
    let romola = book(&b, "Romola").await;
    let downloaded = cover_file_from(&b, romola, crate::images::CoverOrigin::Url).await;
    a.engine
        .storage()
        .enrich_book(
            book(&a, "Romola").await,
            &Book {
                cover_url: Some("https://covers.example.test/2.jpg".into()),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();
    sync(&a, &server).await;
    let got = sync(&b, &server).await;
    assert!(!downloaded.exists());
    assert!(got.covers_to_fetch.contains(&romola), "{got:?}");
    assert!(epub_cover.exists());
}

async fn reopen(d: Device) -> Device {
    let Device { _dir, root, engine } = d;
    drop(engine);
    let engine = Engine::open(EngineConfig::rooted_at(&root)).await.unwrap();
    Device { _dir, root, engine }
}

#[tokio::test]
async fn what_a_failed_sync_left_to_do_is_done_after_a_restart() {
    use crate::storage::Source;

    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    let code = create(&a, &server).await;
    join(&b, &server, &code).await;
    let middlemarch = add_book(&a, "Middlemarch").await;
    let romola = add_book(&a, "Romola").await;
    a.engine
        .storage()
        .enrich_book(
            middlemarch,
            &Book {
                cover_url: Some("https://covers.example.test/1.jpg".into()),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();
    converge(&[&a, &b], &server).await;
    let files = [
        cover_file_from(
            &b,
            book(&b, "Middlemarch").await,
            crate::images::CoverOrigin::Url,
        )
        .await,
        cover_file(&b, book(&b, "Romola").await).await,
    ];

    a.engine.delete_book(romola).await.unwrap();
    a.engine
        .storage()
        .enrich_book(
            middlemarch,
            &Book {
                cover_url: Some("https://covers.example.test/2.jpg".into()),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();
    sync(&a, &server).await;
    let mut failing = Receiver::default();
    failing.faults.fail_caught_up = true;
    assert!(sync_with(&b, &server, failing).await.is_err());
    assert_eq!(rows(&b, "SELECT title FROM books").await, ["Middlemarch"]);

    let b = reopen(b).await;
    let first = sync(&b, &server).await;
    sync(&b, &server).await;
    for file in &files {
        assert!(!file.exists(), "{}", file.display());
    }
    assert_eq!(first.covers_to_fetch, [book(&b, "Middlemarch").await]);
}

#[tokio::test]
async fn a_connection_a_sync_leaves_unsound_is_closed_and_the_next_sync_opens_another() {
    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    let code = create(&a, &server).await;
    join(&b, &server, &code).await;
    converge(&[&a, &b], &server).await;
    add_book(&a, "Middlemarch").await;
    sync(&a, &server).await;

    let opened = b.engine.sync.held.lock().await.opened;
    let mut ending = Receiver::default();
    ending.faults.end_transaction = true;
    let result = sync_with(&b, &server, ending).await;
    assert!(
        matches!(result, Err(EngineError::Sync(SyncError::Interrupted))),
        "{result:?}"
    );
    assert!(
        writes_at_once(&b).await,
        "the unsound connection still held the database"
    );

    let mut holding = Receiver::default();
    holding.faults.hold_write_lock = true;
    let result = sync_with(&b, &server, holding).await;
    assert!(
        matches!(result, Err(EngineError::Sync(SyncError::Interrupted))),
        "{result:?}"
    );
    assert!(
        writes_at_once(&b).await,
        "the unsound connection still held the write lock"
    );
    match b.engine.sync_status().await.unwrap() {
        SyncStatus::On(state) => {
            assert_eq!(state.last_problem, Some(SyncError::Interrupted));
        }
        other => panic!("{other:?}"),
    }
    add_book(&b, "Romola").await;

    sync(&b, &server).await;
    assert!(b.engine.sync.held.lock().await.opened > opened);
    converge(&[&a, &b], &server).await;
    for d in [&a, &b] {
        assert_eq!(
            rows(d, "SELECT title FROM books ORDER BY title").await,
            ["Middlemarch", "Romola"]
        );
    }
    match b.engine.sync_status().await.unwrap() {
        SyncStatus::On(state) => assert_eq!(state.last_problem, None),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_database_copied_without_its_identity_needs_it_and_does_not_sync() {
    let server = TestServer::default();
    let a = device().await;
    let code = create(&a, &server).await;
    add_book(&a, "Middlemarch").await;
    sync(&a, &server).await;

    let identity = &a.engine.config.sync_identity;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(identity).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let SyncStatus::On(state) = a.engine.sync_status().await.unwrap() else {
        panic!("not on");
    };
    assert_eq!(state.server, "https://sync.example.test");
    assert_eq!(state.device_label, "Study");
    assert!(state.log_position > 0);
    assert_eq!(
        a.engine.sync_recovery_code().await.unwrap().as_str(),
        code.as_str()
    );

    let copy = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(copy.path().join("database")).unwrap();
    std::fs::copy(
        a.root.join("database/app.db"),
        copy.path().join("database/app.db"),
    )
    .unwrap();
    let copied = Engine::open(EngineConfig::rooted_at(copy.path()))
        .await
        .unwrap();
    assert_eq!(
        copied.sync_status().await.unwrap(),
        SyncStatus::NeedsCredentials
    );
    let requests = server.requests();
    let refused = copied
        .sync_now_with(|_| Ok(server.clone()), Receiver::default())
        .await;
    assert!(matches!(
        refused,
        Err(EngineError::Sync(SyncError::NeedsCredentials))
    ));
    assert!(matches!(
        copied.sync_recovery_code().await,
        Err(EngineError::Sync(SyncError::NeedsCredentials))
    ));
    assert_eq!(server.requests(), requests);
    assert!(
        rows_of(
            &copied,
            "SELECT name FROM sqlite_master WHERE name LIKE 'fongo!_%' ESCAPE '!'"
        )
        .await
        .len()
            > 1
    );

    let shown = [
        format!("{code:?}"),
        format!("{:?}", a.engine.sync_status().await.unwrap()),
        format!("{:?}", sync(&a, &server).await),
    ];
    let secret = code.as_str();
    let groups: Vec<&str> = secret.split('-').collect();
    for text in &shown {
        assert!(!text.contains(secret), "{text}");
        assert!(groups.iter().all(|g| !text.contains(g)), "{text}");
    }
}

async fn rows_of(engine: &Engine, sql: &str) -> Vec<String> {
    sqlx::query_scalar(sql)
        .fetch_all(engine.storage().pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_library_never_synced_is_as_it_was_and_refuses_what_needs_sync() {
    let server = TestServer::default();
    let a = device().await;
    assert_eq!(a.engine.sync_status().await.unwrap(), SyncStatus::Off);
    for refused in [
        a.engine
            .sync_now_with(|_| Ok(server.clone()), Receiver::default())
            .await
            .map(|_| ()),
        a.engine.sync_recovery_code().await.map(|_| ()),
    ] {
        assert!(matches!(refused, Err(EngineError::Sync(SyncError::Off))));
    }

    let bad = "HUNTER-22222-33333";
    let joined = a
        .engine
        .join_sync_library_via(origin(), bad, "Study", &server)
        .await;
    let Err(error) = joined else {
        panic!("joined with a bad code");
    };
    assert!(matches!(error, EngineError::Sync(SyncError::CodeInvalid)));
    assert!(!error.to_string().contains("HUNTER"));
    for label in ["", "   ", "two\nlines"] {
        let refused = a
            .engine
            .create_sync_library_via(origin(), label, &server)
            .await;
        assert!(matches!(refused, Err(EngineError::InvalidInput(_))));
    }
    assert_eq!(server.requests(), 0);
    assert!(!a.engine.config.sync_identity.exists());
    assert_eq!(
        rows(
            &a,
            "SELECT name FROM sqlite_master WHERE name LIKE 'fongo!_%' ESCAPE '!'"
        )
        .await,
        Vec::<String>::new()
    );
    assert_eq!(a.engine.sync_status().await.unwrap(), SyncStatus::Off);

    create(&a, &server).await;
    let again = a
        .engine
        .create_sync_library_via(origin(), "Study", &server)
        .await;
    assert!(matches!(
        again,
        Err(EngineError::Sync(SyncError::AlreadyOn))
    ));

    let dir = tempfile::tempdir().unwrap();
    let memory = Engine::open(EngineConfig::in_memory(dir.path()))
        .await
        .unwrap();
    let refused = memory
        .create_sync_library_via(origin(), "Study", &server)
        .await;
    assert!(matches!(refused, Err(EngineError::InvalidInput(_))));
    assert_eq!(memory.sync_status().await.unwrap(), SyncStatus::Off);
}

const SENTINEL: &str = "the contents of a file outside the vault";

/// Notes `d` writes straight into its tables, as a device with another idea of paths might.
async fn raw_note(d: &Device, file_path: &str, body: &str) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO notes (file_path, title, kind, created_at, last_modified)
         VALUES (?, 'Prelude', 'note', 1, 1) RETURNING id",
    )
    .bind(file_path)
    .fetch_one(d.engine.storage().pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO note_bodies (note_id, body) VALUES (?, ?)")
        .bind(id)
        .bind(body)
        .execute(d.engine.storage().pool())
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn of_two_notes_whose_paths_differ_only_in_case_one_alone_has_the_file() {
    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    let code = create(&a, &server).await;
    join(&b, &server, &code).await;
    raw_note(&a, "notes/eliot/prelude.md", "the second note's words").await;
    raw_note(&a, "Notes/Eliot/Prelude.md", "the first note's own words").await;
    sync(&a, &server).await;
    let got = sync(&b, &server).await;
    let mut notes = Vec::new();
    for path in ["Notes/Eliot/Prelude.md", "notes/eliot/prelude.md"] {
        notes.push(
            b.engine
                .storage()
                .note_by_path(path)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    let [keeper, twin] = <[_; 2]>::try_from(notes).unwrap();
    assert_eq!(got.refused_notes, [twin.id]);
    let indexed = |words| {
        let b = &b;
        async move {
            b.engine
                .search_marks(words, Some(SearchSource::Note), None, 10)
                .await
                .unwrap()
                .len()
        }
    };
    assert_eq!(indexed("second note").await, 0, "the twin was indexed");
    assert_eq!(indexed("first note").await, 1);
    let file = b.engine.note_path(&keeper).unwrap();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "the first note's own words\n").unwrap();
    assert!(
        b.engine
            .update_note_body(&twin, "overwritten")
            .await
            .is_err()
    );
    assert!(b.engine.refresh_note_from_disk(&twin).await.is_err());
    b.engine.delete_note(&twin).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "the first note's own words\n"
    );
    let found = b
        .engine
        .search_marks("words", Some(SearchSource::Note), None, 10)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
}

#[tokio::test]
async fn a_device_cannot_write_a_note_path_others_would_refuse() {
    let a = device().await;
    for path in crate::notes::hostile_note_paths() {
        let refused = a
            .engine
            .storage()
            .insert_note(
                NewNoteMeta {
                    book_id: None,
                    reading_id: None,
                    highlight_id: None,
                    page: None,
                    location: None,
                    file_path: &path,
                    title: "Prelude",
                    kind: "note",
                },
                "",
                &[],
            )
            .await;
        assert!(
            matches!(refused, Err(EngineError::InvalidInput(_))),
            "{path:?}"
        );
    }
}

/// One file on macOS: É precomposed, E with a combining accent, é. By bytes the second sorts
/// first, then the first.
const ONE_FILE: [(&str, &str); 3] = [
    ("notes/\u{c9}.md", "alpha"),
    ("notes/E\u{301}.md", "beta"),
    ("notes/\u{e9}.md", "gamma"),
];

async fn indexed(d: &Device, words: &str) -> usize {
    d.engine
        .search_marks(words, Some(SearchSource::Note), None, 10)
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn notes_that_are_one_file_have_one_owner_whatever_order_they_arrive_in() {
    let orders = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    for order in orders {
        let server = TestServer::default();
        let (a, b) = (device().await, device().await);
        let code = create(&a, &server).await;
        join(&b, &server, &code).await;
        let mut last = SyncOutcome::default();
        for &i in &order {
            let (path, words) = ONE_FILE[i];
            raw_note(&a, path, words).await;
            sync(&a, &server).await;
            last = sync(&b, &server).await;
        }
        let owner_b = b
            .engine
            .storage()
            .note_by_path(ONE_FILE[1].0)
            .await
            .unwrap()
            .unwrap();
        let mut others_b = Vec::new();
        for i in [0, 2] {
            others_b.push(
                b.engine
                    .storage()
                    .note_by_path(ONE_FILE[i].0)
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        let mut expected: Vec<i64> = others_b.iter().map(|n| n.id).collect();
        expected.sort_unstable();
        assert_eq!(last.refused_notes, expected, "order {order:?}");
        assert_eq!(
            (
                indexed(&b, "alpha").await,
                indexed(&b, "beta").await,
                indexed(&b, "gamma").await
            ),
            (0, 1, 0),
            "order {order:?}"
        );
        let on_a = sync(&a, &server).await;
        assert_eq!(on_a.refused_notes.len(), 2, "order {order:?}");

        let file = b.engine.note_path(&owner_b).unwrap();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "beta\n").unwrap();
        for other in &others_b {
            assert!(
                b.engine
                    .update_note_body(other, "overwritten")
                    .await
                    .is_err()
            );
        }
        b.engine.delete_note(&others_b[1]).await.unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "beta\n");
        assert!(matches!(
            b.engine
                .storage()
                .insert_note(
                    NewNoteMeta {
                        book_id: None,
                        reading_id: None,
                        highlight_id: None,
                        page: None,
                        location: None,
                        file_path: "Notes/E\u{301}.MD",
                        title: "Prelude",
                        kind: "note",
                    },
                    "",
                    &[],
                )
                .await,
            Err(EngineError::InvalidInput(_))
        ));

        // Deleting the owner hands the file to the next, everywhere.
        let owner_a = a
            .engine
            .storage()
            .note_by_path(ONE_FILE[1].0)
            .await
            .unwrap()
            .unwrap();
        a.engine.delete_note(&owner_a).await.unwrap();
        assert_eq!(
            indexed(&a, "alpha").await,
            1,
            "the next note was not handed the file"
        );
        converge(&[&a, &b], &server).await;
        for d in [&a, &b] {
            assert_eq!(indexed(d, "alpha").await, 1, "order {order:?}");
            assert_eq!(indexed(d, "beta").await + indexed(d, "gamma").await, 0);
        }
        assert!(
            sync(&b, &server).await.refused_notes.is_empty(),
            "order {order:?}"
        );
    }
}

#[tokio::test]
async fn a_connection_closed_after_holding_the_write_lock_lets_the_next_writer_in_at_once() {
    let server = TestServer::default();
    let a = device().await;
    create(&a, &server).await;
    let mut probe = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(a.root.join("database/app.db"))
        .busy_timeout(std::time::Duration::ZERO)
        .connect()
        .await
        .unwrap();
    let mut held = 0;
    for _ in 0..40 {
        let mut holding = Receiver::default();
        holding.faults.hold_write_lock = true;
        let result = sync_with(&a, &server, holding).await;
        assert!(
            matches!(result, Err(EngineError::Sync(SyncError::Interrupted))),
            "{result:?}"
        );
        if sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut probe)
            .await
            .is_ok()
        {
            sqlx::query("ROLLBACK").execute(&mut probe).await.unwrap();
        } else {
            held += 1;
        }
    }
    assert_eq!(
        held, 0,
        "the write lock outlived the sync {held} times in 40"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn two_notes_whose_paths_the_key_tells_apart_but_the_disk_does_not_share_nothing() {
    let a = device().await;
    let mut notes = Vec::new();
    for path in ["notes/a.md", "notes/b.md"] {
        let id = a
            .engine
            .storage()
            .insert_note(
                NewNoteMeta {
                    book_id: None,
                    reading_id: None,
                    highlight_id: None,
                    page: None,
                    location: None,
                    file_path: path,
                    title: "Prelude",
                    kind: "note",
                },
                "",
                &[],
            )
            .await
            .unwrap();
        notes.push(a.engine.storage().get_note(id).await.unwrap().unwrap());
    }
    let owner = a.engine.note_path(&notes[0]).unwrap();
    std::fs::create_dir_all(owner.parent().unwrap()).unwrap();
    std::fs::write(&owner, "the first note's words\n").unwrap();
    std::fs::hard_link(&owner, a.engine.note_path(&notes[1]).unwrap()).unwrap();

    assert!(
        a.engine
            .update_note_body(&notes[1], "overwritten")
            .await
            .is_err()
    );
    assert!(a.engine.refresh_note_from_disk(&notes[1]).await.is_err());
    a.engine.delete_note(&notes[1]).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(&owner).unwrap(),
        "the first note's words\n"
    );
    a.engine
        .update_note_body(&notes[0], "the first note, edited")
        .await
        .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn a_note_whose_path_leaves_the_vault_is_kept_but_never_touches_a_file() {
    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    let code = create(&a, &server).await;
    join(&b, &server, &code).await;

    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("x.md"), SENTINEL).unwrap();
    std::fs::write(b.root.join("x.md"), SENTINEL).unwrap();
    let vault = b.engine.vault_dir().to_path_buf();
    std::os::unix::fs::symlink(outside.path(), vault.join("outside")).unwrap();
    let sentinels = [outside.path().join("x.md"), b.root.join("x.md")];

    let mut hostile = crate::notes::hostile_note_paths();
    hostile.push("outside/x.md".into());
    for path in &hostile {
        raw_note(&a, path, "a body that came with the row").await;
    }
    raw_note(
        &a,
        "notes/eliot/middlemarch.md",
        "Miss Brooke had that kind of beauty",
    )
    .await;
    sync(&a, &server).await;
    let got = sync(&b, &server).await;
    assert!(got.held.is_empty(), "{:?}", got.held);
    assert_eq!(got.refused_notes.len(), hostile.len(), "{got:?}");

    let good = b
        .engine
        .storage()
        .note_by_path("notes/eliot/middlemarch.md")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        b.engine.note_path(&good).unwrap(),
        vault.join("notes/eliot/middlemarch.md")
    );
    let found = b
        .engine
        .search_marks("beauty", Some(SearchSource::Note), None, 10)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    let none = b
        .engine
        .search_marks("came with the row", Some(SearchSource::Note), None, 10)
        .await
        .unwrap();
    assert!(none.is_empty(), "{none:?}");

    for &id in &got.refused_notes {
        let note = b.engine.storage().get_note(id).await.unwrap().unwrap();
        assert!(b.engine.note_path(&note).is_err());
        assert!(b.engine.note_body(&note).is_err());
        assert!(b.engine.refresh_note_from_disk(&note).await.is_err());
        assert!(
            b.engine
                .update_note_body(&note, "overwritten")
                .await
                .is_err()
        );
    }
    // Newer than any row, so a sweep that resolved these paths would read them.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    for sentinel in &sentinels {
        std::fs::File::options()
            .write(true)
            .open(sentinel)
            .unwrap()
            .set_modified(later)
            .unwrap();
    }
    b.engine.reconcile_vault().await.unwrap();
    let swept = rows(&b, "SELECT body FROM note_bodies").await;
    assert!(
        swept.iter().all(|body| !body.contains(SENTINEL)),
        "{swept:?}"
    );
    assert!(
        rows(&b, "SELECT body FROM notes_fts")
            .await
            .iter()
            .all(|b| !b.contains(SENTINEL))
    );
    for &id in &got.refused_notes {
        let note = b.engine.storage().get_note(id).await.unwrap().unwrap();
        b.engine.delete_note(&note).await.unwrap();
        for sentinel in &sentinels {
            assert_eq!(
                std::fs::read_to_string(sentinel).unwrap(),
                SENTINEL,
                "after deleting {:?}",
                note.file_path
            );
        }
    }

    converge(&[&a, &b], &server).await;
    for d in [&a, &b] {
        assert!(rows(d, "SELECT title FROM notes_fts").await.len() <= 1);
        let bodies = rows(d, "SELECT body FROM note_bodies").await;
        assert!(
            bodies.iter().all(|body| !body.contains(SENTINEL)),
            "{bodies:?}"
        );
    }
}

/// A pull larger than one apply is committed in several transactions, each of whole envelopes
/// and no more changes than the pace allows; a write the reader makes while one is open goes
/// through; and when the log holds more than one call reads, the next call starts from how long
/// the one before held the write lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_pull_takes_turns_with_the_readers_writes() {
    use std::sync::{Arc, Mutex};

    use fongo_sync_client::Apply;

    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    let code = create(&a, &server).await;
    sync(&a, &server).await;
    sqlx::query("INSERT INTO dismissed_shelves (source, tag, dismissed_at) VALUES ('goodreads', 'to-read', 1)")
        .execute(a.engine.storage().pool())
        .await
        .unwrap();
    sync(&a, &server).await;
    // One call reads at most 1,000 pages of the test server's two envelopes.
    server.repeat_last(2_100);
    join(&b, &server, &code).await;

    let timings = Arc::new(Mutex::new(Vec::new()));
    let paces = Arc::new(Mutex::new(Vec::new()));
    let (opened, wait) = tokio::sync::oneshot::channel();
    let mut receiver = Receiver::default();
    receiver.faults.timings = Some(timings.clone());
    receiver.faults.paces = Some(paces.clone());
    receiver.faults.in_first_apply = Some(Box::new(move || opened.send(()).unwrap()));
    let write = async {
        wait.await.unwrap();
        b.engine.create_list("Written while syncing", None).await
    };
    let (synced, written) = tokio::join!(sync_with(&b, &server, receiver), write);
    synced.unwrap();
    written.unwrap();
    assert_eq!(
        rows(&b, "SELECT name FROM book_lists").await,
        ["Written while syncing"]
    );

    let limit = super::PACE_APPLY;
    assert_eq!(Receiver::default().pace().apply, limit);
    let sizes: Vec<usize> = timings
        .lock()
        .unwrap()
        .iter()
        .map(|t| t.changes)
        .filter(|&n| n > 0)
        .collect();
    assert!(sizes.len() >= 3, "{sizes:?}");
    assert!(sizes.iter().all(|&n| n <= limit), "{sizes:?}");
    let paces = paces.lock().unwrap().clone();
    assert_eq!(paces.len(), 2, "{paces:?}");
    // The first call starts from what joining just held, a single short step.
    assert!(paces[0] < Receiver::default().pace().hold, "{paces:?}");
    assert!(paces[1] > std::time::Duration::ZERO, "{paces:?}");
}

/// Calls `op` every 20 ms until `stop`; the longest call, the failures and the calls made.
fn party<F, Fut>(
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    mut op: F,
) -> tokio::task::JoinHandle<Party>
where
    F: FnMut(usize) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = crate::Result<()>> + Send,
{
    tokio::spawn(async move {
        let mut party = Party::default();
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            let started = std::time::Instant::now();
            if let Err(e) = op(party.calls).await {
                party.failed += 1;
                party.error.get_or_insert(e.to_string());
            }
            if started.elapsed() > party.worst {
                party.worst = started.elapsed();
                party.worst_began = Some(started);
            }
            party.calls += 1;
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        party
    })
}

#[derive(Debug, Default)]
struct Party {
    worst: std::time::Duration,
    worst_began: Option<std::time::Instant>,
    failed: usize,
    calls: usize,
    error: Option<String>,
}

/// How a large first sync shares the database with everything else that uses it: a device joins
/// a library of `n` books, each with a highlight and a note, and syncs while the reader's engine
/// writes and reads through its API every 20 ms and a second engine on the same data directory
/// writes every 20 ms. A measurement, not a check. In a release build:
/// `BOOKMOLE_SYNC_BENCH_BOOKS=10000,50000 BOOKMOLE_SYNC_BENCH_RUNS=5 cargo test -p bookmole
/// --release --lib -- --ignored --nocapture a_large_first_sync`.
///
/// Each run prints how long the sync took over how many applies, its longest hold on the write
/// lock (an apply and its commit, less any pause the client took before the next write), the
/// kernel's and the repair's shares of the holds, and for each party its worst wait and its
/// failures.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a measurement; run in a release build"]
#[expect(clippy::too_many_lines, reason = "one measurement, read top to bottom")]
async fn a_large_first_sync_while_the_reader_works() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use crate::{BookQuery, BookSort};

    let env = |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.into());
    let sizes: Vec<i64> = env("BOOKMOLE_SYNC_BENCH_BOOKS", "10000")
        .split(',')
        .filter_map(|n| n.trim().parse().ok())
        .collect();
    let runs: usize = env("BOOKMOLE_SYNC_BENCH_RUNS", "1").parse().unwrap();
    for n in sizes {
        for run in 0..runs {
            let server = TestServer::default();
            let a = device().await;
            let code = create(&a, &server).await;
            for sql in [
                "WITH RECURSIVE i(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM i WHERE n < ?1)
                 INSERT INTO books (title, authors, cover_url, created_at, last_modified)
                 SELECT 'Book ' || n, '[\"George Eliot\"]',
                        'https://covers.example.test/' || n || '.jpg', 1, 1
                   FROM i",
                "INSERT INTO highlights (book_id, text, chapter, page, source, identity_hash,
                                         created_at)
                 SELECT id, 'Who can quit young lives after being long in company with them? '
                            || id, 'Finale', 838, 'koreader', 'bench-' || id, 1
                   FROM books",
                // One path in ten is not ASCII, as accented titles make them.
                "INSERT INTO notes (book_id, file_path, title, kind, created_at, last_modified)
                 SELECT id, CASE WHEN id % 10 = 0 THEN 'notes/Études ' ELSE 'notes/Book ' END
                            || id || '.md', 'Book ' || id, 'note', 1, 1
                   FROM books",
                "INSERT INTO note_bodies (note_id, body)
                 SELECT id, 'Miss Brooke had that kind of beauty which seems to be thrown into
                             relief by poor dress. See [[Book ' || (id + 1) || ']].'
                   FROM notes",
            ] {
                sqlx::query(sql)
                    .bind(n)
                    .execute(a.engine.storage().pool())
                    .await
                    .unwrap();
            }
            let started = Instant::now();
            sync(&a, &server).await;
            let sent = started.elapsed();
            drop(a);

            let dir = tempfile::tempdir().unwrap();
            let engine = Arc::new(
                Engine::open(EngineConfig::rooted_at(dir.path()))
                    .await
                    .unwrap(),
            );
            engine
                .join_sync_library_via(origin(), code.as_str(), "Reading chair", &server)
                .await
                .unwrap();
            let beside = Arc::new(
                Engine::open(EngineConfig::rooted_at(dir.path()))
                    .await
                    .unwrap(),
            );
            let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
                .fetch_one(engine.storage().pool())
                .await
                .unwrap();

            let stop = Arc::new(AtomicBool::new(false));
            let writer = party(stop.clone(), {
                let engine = engine.clone();
                move |i| {
                    let engine = engine.clone();
                    async move {
                        let name = format!("Written while syncing {i}");
                        engine.create_list(&name, None).await.map(|_| ())
                    }
                }
            });
            let reader = party(stop.clone(), {
                let engine = engine.clone();
                move |_| {
                    let engine = engine.clone();
                    async move {
                        let page = BookQuery::new(50, BookSort::Title);
                        engine.list_books(&page).await.map(|_| ())
                    }
                }
            });
            let other = party(stop.clone(), {
                let beside = beside.clone();
                move |i| {
                    let beside = beside.clone();
                    async move {
                        let name = format!("Written beside {i}");
                        beside.create_list(&name, None).await.map(|_| ())
                    }
                }
            });

            let timings = Arc::new(Mutex::new(Vec::new()));
            let gaps = Arc::new(Mutex::new(Vec::new()));
            let mut receiver = Receiver::default();
            receiver.faults.timings = Some(timings.clone());
            receiver.faults.gaps = Some(gaps.clone());
            let started = Instant::now();
            engine
                .sync_now_with(|_| Ok(server.clone()), receiver)
                .await
                .unwrap();
            let synced = started.elapsed();
            stop.store(true, Ordering::Relaxed);
            let parties = [
                ("writer", writer.await.unwrap()),
                ("reader", reader.await.unwrap()),
                ("other engine", other.await.unwrap()),
            ];

            // A transaction runs from its apply to the next gap the client leaves, less the
            // client's pause when it took one.
            let pause = Duration::from_millis(150);
            let timings = timings.lock().unwrap().clone();
            let gaps = gaps.lock().unwrap().clone();
            let holds: Vec<Duration> = timings
                .iter()
                .map(|t| {
                    let commit = gaps
                        .iter()
                        .find(|&&g| g >= t.ended)
                        .map_or(Duration::ZERO, |&g| g - t.ended);
                    let commit = commit.checked_sub(pause).unwrap_or(commit);
                    t.ended - t.started + commit
                })
                .collect();
            let longest = holds.iter().max().copied().unwrap_or_default();
            let held: Duration = holds.iter().sum();
            let kernel: Duration = timings.iter().map(|t| t.kernel).sum();
            let repair: Duration = timings.iter().map(|t| t.repair).sum();
            let worst_repair = timings.iter().map(|t| t.repair).max().unwrap_or_default();
            let share = |d: Duration| 100.0 * d.as_secs_f64() / held.as_secs_f64().max(1e-9);
            let mut line = format!(
                "{n} books, run {run}, {journal}: sent in {:.1}s; synced in {:.1}s over {} \
                 applies; longest hold {} ms; kernel {:.0}%, repair {:.0}% of the holds \
                 (longest repair {} ms)",
                sent.as_secs_f64(),
                synced.as_secs_f64(),
                timings.len(),
                longest.as_millis(),
                share(kernel),
                share(repair),
                worst_repair.as_millis(),
            );
            for (name, p) in &parties {
                line += &format!(
                    "; {name} worst {} ms, {} of {} failed",
                    p.worst.as_millis(),
                    p.failed,
                    p.calls
                );
                if let Some(e) = &p.error {
                    line += &format!(" ({e})");
                }
            }
            println!("{line}");
        }
    }
}

/// Which books [`fill`] gives an ISBN-13.
#[derive(Clone, Copy)]
enum Isbns {
    None,
    /// Book `n` of the fill gets the ISBN numbered `n`.
    All,
    /// As `All` for odd `n`; even `n` get ISBNs no other fill gives.
    Odd,
}

/// `books` books, each with a highlight and a note with a body, written as the reader's imports
/// write: a hundred rows to a statement. `name` tells this fill's titles, highlights and note
/// paths from another device's.
async fn fill(engine: &Engine, books: i64, name: &str, isbns: Isbns) {
    let pool = engine.storage().pool();
    let isbn = match isbns {
        Isbns::None => "NULL",
        Isbns::All => "'978' || printf('%010d', n)",
        Isbns::Odd => {
            "'978' || printf('%010d', CASE WHEN n % 2 = 1 THEN n ELSE 1000000000 + n END)"
        }
    };
    for from in (1..=books).step_by(100) {
        let to = (from + 99).min(books);
        let last: i64 = sqlx::query_scalar("SELECT coalesce(max(id), 0) FROM books")
            .fetch_one(pool)
            .await
            .unwrap();
        let statements = [
            format!(
                "WITH RECURSIVE i(n) AS (SELECT ?1 UNION ALL SELECT n + 1 FROM i WHERE n < ?2)
                 INSERT INTO books (title, authors, cover_url, isbn_13, created_at, last_modified)
                 SELECT ?3 || ' ' || n, '[\"George Eliot\"]',
                        'https://covers.example.test/' || ?3 || n || '.jpg', {isbn}, 1, 1
                   FROM i"
            ),
            "INSERT INTO highlights (book_id, text, chapter, page, source, identity_hash,
                                     created_at)
             SELECT id, 'Who can quit young lives after being long in company with them? '
                        || id, 'Finale', 838, 'koreader', 'v2:' || ?3 || '-' || id, 1
               FROM books WHERE id > ?4"
                .to_string(),
            // One path in ten is not ASCII, as accented titles make them.
            "INSERT INTO notes (book_id, file_path, title, kind, created_at, last_modified)
             SELECT id, 'notes/' || CASE WHEN id % 10 = 0 THEN 'Études ' ELSE '' END
                        || ?3 || ' ' || id || '.md', ?3 || ' ' || id, 'note', 1, 1
               FROM books WHERE id > ?4"
                .to_string(),
            "INSERT INTO note_bodies (note_id, body)
             SELECT n.id, 'Miss Brooke had that kind of beauty which seems to be thrown into
                           relief by poor dress. See [[' || ?3 || ' ' || (n.book_id + 1) || ']].'
               FROM notes n WHERE n.book_id > ?4"
                .to_string(),
        ];
        for sql in &statements {
            sqlx::query(sql)
                .bind(from)
                .bind(to)
                .bind(name)
                .bind(last)
                .execute(pool)
                .await
                .unwrap();
        }
    }
}

/// The longest time no connection could take SQLite's write lock, as a connection of its own
/// sees it trying every millisecond without waiting, until `stop`: the longest a writer waiting
/// in the busy handler could have had to wait for its turn.
fn probe_write_lock(
    engine: &Engine,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<std::time::Duration> {
    use std::time::{Duration, Instant};

    let options = (*engine.storage().pool().connect_options())
        .clone()
        .busy_timeout(Duration::ZERO);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut conn = options.connect().await.unwrap();
            let mut free = Instant::now();
            let mut longest = Duration::ZERO;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if sqlx::query("BEGIN IMMEDIATE")
                    .execute(&mut conn)
                    .await
                    .is_ok()
                {
                    sqlx::query("ROLLBACK").execute(&mut conn).await.unwrap();
                    let now = Instant::now();
                    longest = longest.max(now - free);
                    free = now;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            longest.max(free.elapsed())
        })
    })
}

/// How sending a large library shares the database with everything else that uses it, in three
/// cases of `n` books, each with a highlight and a note: `create`, a device that holds them makes
/// a synced library and syncs; `join`, a device that holds them joins a library of `n / 10`
/// books, half of which have the ISBN of one of its own, and syncs; `backlog`, a device of a
/// synced library writes them offline and syncs. Meanwhile the reader's engine writes and reads
/// through its API every 20 ms and a second engine on the same data directory writes every 20 ms.
/// A measurement, not a check. In a release build: `BOOKMOLE_SYNC_BENCH_BOOKS=10000,50000
/// BOOKMOLE_SYNC_BENCH_CASES=create,join,backlog cargo test -p bookmole --release --lib --
/// --ignored --nocapture a_large_first_sync`.
///
/// Each run prints how long each part took, the longest time no other connection could take the
/// write lock, for each party its worst wait and its failures, and how long the work of each
/// kind of transaction took. `BOOKMOLE_SYNC_BENCH_COLLECT` sets the rows a step takes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a measurement; run in a release build"]
#[expect(clippy::too_many_lines, reason = "one measurement, read top to bottom")]
async fn a_large_first_sync_sends_while_the_reader_works() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use crate::{BookQuery, BookSort};

    let env = |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.into());
    let sizes: Vec<i64> = env("BOOKMOLE_SYNC_BENCH_BOOKS", "10000")
        .split(',')
        .filter_map(|n| n.trim().parse().ok())
        .collect();
    let cases = env("BOOKMOLE_SYNC_BENCH_CASES", "create,join,backlog");
    let collect: Option<usize> = std::env::var("BOOKMOLE_SYNC_BENCH_COLLECT")
        .ok()
        .and_then(|n| n.parse().ok());
    for n in sizes {
        for case in cases.split(',').map(str::trim) {
            let server = TestServer::default();
            let dir = tempfile::tempdir().unwrap();
            let engine = Arc::new(
                Engine::open(EngineConfig::rooted_at(dir.path()))
                    .await
                    .unwrap(),
            );
            let library = tempfile::tempdir().unwrap();
            let code = match case {
                "create" => None,
                "join" => {
                    let other = Engine::open(EngineConfig::rooted_at(library.path()))
                        .await
                        .unwrap();
                    let code = other
                        .create_sync_library_via(origin(), "Study", &server)
                        .await
                        .unwrap();
                    fill(&other, n / 10, "Library", Isbns::Odd).await;
                    other
                        .sync_now_with(|_| Ok(server.clone()), Receiver::default())
                        .await
                        .unwrap();
                    Some(code)
                }
                "backlog" => {
                    engine
                        .create_sync_library_via(origin(), "Study", &server)
                        .await
                        .unwrap();
                    engine
                        .sync_now_with(|_| Ok(server.clone()), Receiver::default())
                        .await
                        .unwrap();
                    None
                }
                other => panic!("no case {other}"),
            };
            let started = Instant::now();
            let isbns = if case == "join" {
                Isbns::All
            } else {
                Isbns::None
            };
            fill(&engine, n, "Book", isbns).await;
            let filled = started.elapsed();
            let beside = Arc::new(
                Engine::open(EngineConfig::rooted_at(dir.path()))
                    .await
                    .unwrap(),
            );
            let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
                .fetch_one(engine.storage().pool())
                .await
                .unwrap();
            engine.sync.held.lock().await.step_rows = collect;

            let stop = Arc::new(AtomicBool::new(false));
            let probe = probe_write_lock(&engine, stop.clone());
            let writer = party(stop.clone(), {
                let engine = engine.clone();
                move |i| {
                    let engine = engine.clone();
                    async move {
                        let name = format!("Written while syncing {i}");
                        engine.create_list(&name, None).await.map(|_| ())
                    }
                }
            });
            let reader = party(stop.clone(), {
                let engine = engine.clone();
                move |_| {
                    let engine = engine.clone();
                    async move {
                        let page = BookQuery::new(50, BookSort::Title);
                        engine.list_books(&page).await.map(|_| ())
                    }
                }
            });
            let other = party(stop.clone(), {
                let beside = beside.clone();
                move |i| {
                    let beside = beside.clone();
                    async move {
                        let name = format!("Written beside {i}");
                        beside.create_list(&name, None).await.map(|_| ())
                    }
                }
            });
            tokio::time::sleep(Duration::from_millis(100)).await;

            let started = Instant::now();
            match case {
                "create" => {
                    engine
                        .create_sync_library_via(origin(), "Study", &server)
                        .await
                        .unwrap();
                }
                "join" => {
                    engine
                        .join_sync_library_via(
                            origin(),
                            code.as_ref().unwrap().as_str(),
                            "Reading chair",
                            &server,
                        )
                        .await
                        .unwrap();
                }
                _ => {}
            }
            let turned_on = started.elapsed();
            let steps = Arc::new(std::sync::Mutex::new(Vec::new()));
            let timings = Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut receiver = Receiver::default();
            receiver.faults.steps = Some(steps.clone());
            receiver.faults.timings = Some(timings.clone());
            receiver.faults.collect = collect;
            let outcome = engine.sync_now_with(|_| Ok(server.clone()), receiver).await;
            let total = started.elapsed();
            tokio::time::sleep(Duration::from_millis(100)).await;
            stop.store(true, Ordering::Relaxed);
            let longest = probe.join().unwrap();
            let parties = [
                ("writer", writer.await.unwrap()),
                ("reader", reader.await.unwrap()),
                ("other engine", other.await.unwrap()),
            ];
            let mut line = format!(
                "{case} {n} books, {journal}: filled in {:.1}s; turned on in {:.1}s, synced in \
                 {:.1}s, {:.1}s in all ({}); write lock never free for {} ms",
                filled.as_secs_f64(),
                turned_on.as_secs_f64(),
                (total - turned_on).as_secs_f64(),
                total.as_secs_f64(),
                match &outcome {
                    Ok(o) => format!("sent {}, received {}", o.sent, o.received),
                    Err(e) => format!("failed: {e}"),
                },
                longest.as_millis(),
            );
            for (name, p) in &parties {
                line += &format!(
                    "; {name} worst {} ms at {:.1}s, {} of {} failed",
                    p.worst.as_millis(),
                    p.worst_began
                        .map_or(0.0, |w| w.saturating_duration_since(started).as_secs_f64()),
                    p.failed,
                    p.calls
                );
                if let Some(e) = &p.error {
                    line += &format!(" ({e})");
                }
            }
            // Each transaction's work, its commit left out.
            let held = engine.sync.held.lock().await;
            let installs: Vec<Duration> =
                held.install_steps.iter().map(|(took, _)| *took).collect();
            if let Some((_, ended)) = held.install_steps.first() {
                line += &format!(
                    "; first install step ended at {:.1}s",
                    ended.saturating_duration_since(started).as_secs_f64()
                );
            }
            drop(held);
            let steps: Vec<Duration> = steps
                .lock()
                .unwrap()
                .iter()
                .map(|(t, _)| t.kernel + t.repair)
                .collect();
            let applies: Vec<Duration> = timings
                .lock()
                .unwrap()
                .iter()
                .map(|t| t.kernel + t.repair)
                .collect();
            for (name, took) in [
                ("install steps", &installs),
                ("collect steps", &steps),
                ("applies", &applies),
            ] {
                let longest = took.iter().max().copied().unwrap_or_default();
                let first = took.first().copied().unwrap_or_default();
                line += &format!(
                    "; {} {name}, longest {} ms, first {} ms",
                    took.len(),
                    longest.as_millis(),
                    first.as_millis()
                );
            }
            println!("{line}");
        }
    }
}

/// What `d` sent through `server`, with what differs between two runs renamed: gids in order of
/// first appearance and clocks by rank.
fn sent(d: &Device, server: &TestServer) -> Vec<String> {
    use std::collections::BTreeMap;

    use fongo_sync_sqlite::core::{Gid, Value};

    let super::identity::Stored::Found(identity) =
        super::identity::read(&d.root.join(super::identity::FILE_NAME)).unwrap()
    else {
        panic!("no identity");
    };
    let key = identity.credentials.device().verifying_key();
    let changes: Vec<_> = server
        .envelopes()
        .iter()
        .flat_map(|e| {
            fongo_sync_client::protocol::open(e, identity.credentials.library(), |_| Some(key))
                .unwrap()
                .changes
        })
        .collect();
    let mut clocks: Vec<_> = changes.iter().map(|c| c.clock).collect();
    clocks.sort();
    clocks.dedup();
    let mut gids: BTreeMap<Gid, usize> = BTreeMap::new();
    let mut name = |gid: Gid| {
        let next = gids.len();
        *gids.entry(gid).or_insert(next)
    };
    changes
        .iter()
        .map(|c| {
            let gid = name(c.gid);
            let value = match &c.value {
                Value::Ref(g) => format!("ref {}", name(*g)),
                other => format!("{other:?}"),
            };
            let rank = clocks.binary_search(&c.clock).unwrap();
            format!("{} {gid} {} {value} at {rank}", c.collection, c.field)
        })
        .collect()
}

async fn uncollected(d: &Device) -> usize {
    let mut conn = d.engine.storage().pool().acquire().await.unwrap();
    super::replica()
        .unwrap()
        .uncollected_rows(&mut conn, usize::MAX)
        .await
        .unwrap()
}

/// Turning sync on over a library adopts its rows in several transactions, and a write the
/// reader makes between two of them, while rows still wait to be adopted, goes through and is
/// sent with them.
#[tokio::test]
async fn sync_turned_on_over_a_library_adopts_it_in_steps_and_lets_writes_through() {
    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    fill(&a.engine, 60, "Book", Isbns::None).await;
    let (gaps, mut gap) = tokio::sync::mpsc::unbounded_channel();
    {
        let mut held = a.engine.sync.held.lock().await;
        held.step_rows = Some(40);
        held.gaps = Some(gaps);
    }
    let creating = async {
        let code = create(&a, &server).await;
        a.engine.sync.held.lock().await.gaps = None;
        code
    };
    let answering = async {
        let mut waiting = Vec::new();
        while let Some(done) = gap.recv().await {
            if waiting.is_empty() {
                waiting.push(uncollected(&a).await);
                a.engine
                    .create_list("Written while turning sync on", None)
                    .await
                    .unwrap();
            }
            done.send(()).unwrap();
        }
        waiting
    };
    let (code, waiting) = tokio::join!(creating, answering);
    assert!(waiting[0] > 0, "{waiting:?}");
    let steps = a.engine.sync.held.lock().await.install_steps.len();
    assert!(steps >= 3, "{steps} steps");
    assert_eq!(uncollected(&a).await, 1);

    join(&b, &server, &code).await;
    converge(&[&a, &b], &server).await;
    assert_eq!(scalar::<i64>(&b, "SELECT count(*) FROM books").await, 60);
    assert_eq!(
        scalar::<i64>(&b, "SELECT count(*) FROM note_bodies").await,
        60
    );
    assert_eq!(
        rows(&b, "SELECT name FROM book_lists").await,
        ["Written while turning sync on"]
    );
}

/// A device whose process stopped between two steps of turning sync on carries on in steps
/// once it opens again, and sends what one that never stopped sends.
#[tokio::test]
async fn sync_turned_on_and_stopped_part_way_carries_on_in_steps_after_a_restart() {
    use std::sync::{Arc, Mutex};

    async fn run(stop: Option<usize>) -> (Vec<String>, usize, Vec<usize>) {
        let server = TestServer::default();
        let a = device().await;
        fill(&a.engine, 60, "Book", Isbns::None).await;
        // Made at the first open; the two runs' would differ only by when they ran.
        sqlx::query("UPDATE rating_scales SET created_at = 1")
            .execute(a.engine.storage().pool())
            .await
            .unwrap();
        {
            let mut held = a.engine.sync.held.lock().await;
            held.step_rows = Some(40);
            held.stop_after_steps = stop;
        }
        create(&a, &server).await;
        let installed = a.engine.sync.held.lock().await.install_steps.len();
        let a = reopen(a).await;
        let steps = Arc::new(Mutex::new(Vec::new()));
        let mut receiver = Receiver::default();
        receiver.faults.collect = Some(40);
        receiver.faults.steps = Some(steps.clone());
        sync_with(&a, &server, receiver).await.unwrap();
        assert_eq!(uncollected(&a).await, 0);
        let steps = steps.lock().unwrap().iter().map(|(_, c)| c.rows).collect();
        (sent(&a, &server), installed, steps)
    }

    let (whole, installed, _) = run(None).await;
    assert!(installed >= 3, "{installed}");
    let (stopped, installed, steps) = run(Some(2)).await;
    assert_eq!(installed, 2);
    assert!(steps.len() >= 2, "{steps:?}");
    assert!(steps.iter().all(|&rows| rows < 100), "{steps:?}");
    assert_eq!(stopped.len(), whole.len());
    assert_eq!(stopped, whole);
}

async fn reflection_note(d: &Device, path: &str, kind: &str, made: i64) -> i64 {
    let reading: i64 = scalar(d, "SELECT id FROM readings").await;
    let book: i64 = scalar(d, "SELECT id FROM books").await;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO notes (book_id, reading_id, file_path, title, kind, created_at,
                            last_modified)
         VALUES (?, ?, ?, 'Prelude', ?, ?, ?) RETURNING id",
    )
    .bind(book)
    .bind(reading)
    .bind(path)
    .bind(kind)
    .bind(made)
    .bind(made)
    .fetch_one(d.engine.storage().pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO note_bodies (note_id, body) VALUES (?, 'Dorothea')")
        .bind(id)
        .execute(d.engine.storage().pool())
        .await
        .unwrap();
    id
}

async fn unhistoric(d: &Device) -> usize {
    d.engine
        .search_marks("unhistoric", Some(SearchSource::Note), None, 10)
        .await
        .unwrap()
        .len()
}

async fn run_on(d: &Device, sql: &str, id: i64) {
    sqlx::query(sql)
        .bind(id)
        .execute(d.engine.storage().pool())
        .await
        .unwrap();
}

/// Another device makes a note into a reflection, which waits on `a` behind `a`'s own
/// reflection on the same reading, until the reader on `a` makes theirs a plain note and edits
/// the waiting one. Returns the waiting note's id on `a`; nothing on `a` is collected since.
async fn a_reflection_freed_here(server: &TestServer) -> (Device, i64) {
    let (a, b) = (device().await, device().await);
    let code = create(&a, server).await;
    join(&b, server, &code).await;
    let book = add_book(&a, "Middlemarch").await;
    a.engine
        .storage()
        .open_reading(book, Some(100), "manual")
        .await
        .unwrap();
    converge(&[&a, &b], server).await;
    reflection_note(&a, "Middlemarch/finale.md", "note", 2000).await;
    converge(&[&a, &b], server).await;

    let on_b: i64 = scalar(&b, "SELECT id FROM notes").await;
    run_on(
        &b,
        "UPDATE notes SET kind = 'reflection', title = 'Unhistoric acts' WHERE id = ?",
        on_b,
    )
    .await;
    let ours = reflection_note(&a, "Middlemarch/reflection.md", "reflection", 1000).await;
    sync(&b, server).await;
    let got = sync(&a, server).await;
    assert!(got.held.iter().any(|h| h.collection == "notes"), "{got:?}");
    assert_eq!(unhistoric(&a).await, 0);

    let waiting: i64 = scalar(&a, "SELECT id FROM notes WHERE kind = 'note'").await;
    run_on(&a, "UPDATE notes SET kind = 'note' WHERE id = ?", ours).await;
    run_on(&a, "UPDATE notes SET page = 838 WHERE id = ?", waiting).await;
    (a, waiting)
}

fn writes(collected: &fongo_sync_sqlite::Collected, note: i64) -> bool {
    collected
        .touched
        .iter()
        .any(|t| t.collection == "notes" && t.key == format!("[{note}]") && !t.deleted)
}

/// A step of collecting that writes a row held back before brings what this device derives
/// from it up to date in the step's own transaction: the freed reflection is found by its new
/// title once the step commits.
#[tokio::test]
async fn a_row_a_step_of_collecting_writes_is_repaired_with_it() {
    use fongo_sync_client::Apply;

    let server = TestServer::default();
    let (a, waiting) = a_reflection_freed_here(&server).await;
    let replica = super::replica().unwrap();
    let mut conn = a.engine.storage().pool().acquire().await.unwrap();
    let mut tx = conn.begin().await.unwrap();
    let collected = Receiver::default()
        .collect(&mut tx, &replica, 1)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    drop(conn);
    assert!(writes(&collected, waiting), "{collected:?}");
    assert_eq!(unhistoric(&a).await, 1);
}

/// The same, with much written offline after the edits that free the reflection: the sync's
/// first step writes it, the rest follow in steps, and the sync reports something received.
#[tokio::test]
async fn a_row_the_first_step_of_a_sync_writes_is_repaired_and_reported() {
    use std::sync::{Arc, Mutex};

    let server = TestServer::default();
    let (a, waiting) = a_reflection_freed_here(&server).await;
    for i in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        add_book(&a, &format!("Written offline {i}")).await;
    }
    let steps = Arc::new(Mutex::new(Vec::new()));
    let mut receiver = Receiver::default();
    receiver.faults.collect = Some(5);
    receiver.faults.steps = Some(steps.clone());
    let got = sync_with(&a, &server, receiver).await.unwrap();
    assert!(got.held.is_empty(), "{got:?}");
    assert!(got.received, "{got:?}");
    let steps = steps.lock().unwrap().clone();
    assert!(steps.len() >= 3, "{} steps", steps.len());
    let wrote = steps.iter().position(|(_, c)| writes(c, waiting));
    assert_eq!(wrote, Some(0), "{steps:?}");
    assert_eq!(unhistoric(&a).await, 1);
}

/// A database whose sync tables are of the layout the kernel used before installing in steps
/// opens, is upgraded in place by its next sync, and sends what waited: changes already in its
/// outbox and a write not yet collected.
#[tokio::test]
async fn a_library_of_the_earlier_sync_layout_is_upgraded_and_keeps_its_outbox() {
    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    let code = create(&a, &server).await;
    join(&b, &server, &code).await;
    converge(&[&a, &b], &server).await;

    add_book(&a, "Middlemarch").await;
    {
        let mut conn = a.engine.storage().pool().acquire().await.unwrap();
        let replica = super::replica().unwrap();
        replica.collect(&mut conn).await.unwrap();
        assert!(
            !replica
                .outbox(&mut conn, 10)
                .await
                .unwrap()
                .changes
                .is_empty()
        );
    }
    add_book(&a, "Romola").await;
    // As the kernel's layout 3 laid its tables down: without the found rows' table, its index,
    // or the index of marks in collecting order.
    for sql in [
        "DROP TABLE fongo_found",
        "DROP INDEX fongo_dirty_order",
        "UPDATE fongo_meta SET value = 3 WHERE key = 'layout'",
    ] {
        sqlx::query(sql)
            .execute(a.engine.storage().pool())
            .await
            .unwrap();
    }
    let a = reopen(a).await;
    assert_eq!(
        scalar::<i64>(&a, "SELECT value FROM fongo_meta WHERE key = 'layout'").await,
        3
    );
    let got = sync(&a, &server).await;
    assert!(got.sent, "{got:?}");
    assert_eq!(
        scalar::<i64>(&a, "SELECT value FROM fongo_meta WHERE key = 'layout'").await,
        4
    );
    assert_eq!(
        scalar::<i64>(
            &a,
            "SELECT count(*) FROM sqlite_master
              WHERE name IN ('fongo_found', 'fongo_found_batch', 'fongo_dirty_order')"
        )
        .await,
        3
    );
    converge(&[&a, &b], &server).await;
    assert_eq!(
        rows(&b, "SELECT title FROM books ORDER BY title").await,
        ["Middlemarch", "Romola"]
    );
}

/// Turning sync on leaves the write lock free for the pace's pause between its steps once it
/// has held it for the pace's hold, and a sync begun straight after starts from what the
/// install held, so it pauses before its first step too.
#[tokio::test]
async fn turning_sync_on_and_the_sync_after_it_leave_the_lock_free_for_the_pause() {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    let pause = Duration::from_millis(200);
    let pace = fongo_sync_client::Pace {
        hold: Duration::ZERO,
        pause,
        ..fongo_sync_client::Pace::default()
    };
    let server = TestServer::default();
    let a = device().await;
    fill(&a.engine, 30, "Book", Isbns::None).await;
    {
        let mut held = a.engine.sync.held.lock().await;
        held.step_rows = Some(40);
        held.pace = Some(pace);
    }
    create(&a, &server).await;
    let steps = Arc::new(Mutex::new(Vec::new()));
    let mut receiver = Receiver::default();
    receiver.faults.pace = Some(pace);
    receiver.faults.steps = Some(steps.clone());
    sync_with(&a, &server, receiver).await.unwrap();

    let installed = a.engine.sync.held.lock().await.install_steps.clone();
    assert!(installed.len() >= 3, "{installed:?}");
    let mut gaps: Vec<Duration> = installed
        .windows(2)
        .map(|w| (w[1].1 - w[1].0).saturating_duration_since(w[0].1))
        .collect();
    let first = steps.lock().unwrap()[0].0.started;
    gaps.push(first.saturating_duration_since(installed.last().unwrap().1));
    assert!(gaps.iter().all(|&gap| gap >= pause), "{gaps:?}");
}

/// A create whose first step fails leaves sync off and no identity behind, so it can be tried
/// again.
#[tokio::test]
async fn a_create_whose_install_fails_leaves_sync_off() {
    let server = TestServer::default();
    let a = device().await;
    let rename = |from: &str, to: &str| {
        format!("ALTER TABLE dismissed_shelves RENAME COLUMN {from} TO {to}")
    };
    sqlx::query(&rename("dismissed_at", "dismissed_on"))
        .execute(a.engine.storage().pool())
        .await
        .unwrap();
    let refused = a
        .engine
        .create_sync_library_via(origin(), "Study", &server)
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(!a.root.join(super::identity::FILE_NAME).exists());
    assert_eq!(a.engine.sync_status().await.unwrap(), SyncStatus::Off);

    sqlx::query(&rename("dismissed_on", "dismissed_at"))
        .execute(a.engine.storage().pool())
        .await
        .unwrap();
    create(&a, &server).await;
    assert!(matches!(
        a.engine.sync_status().await.unwrap(),
        SyncStatus::On(_)
    ));
}

async fn kernel_tables(d: &Device) -> Vec<String> {
    rows(
        d,
        "SELECT name FROM sqlite_master WHERE name LIKE 'fongo!_%' ESCAPE '!'",
    )
    .await
}

async fn add_edition(d: &Device, title: &str, isbn_13: &str) -> i64 {
    let book = Book {
        title: Some(title.into()),
        authors: vec!["George Eliot".into()],
        isbn_13: Some(isbn_13.into()),
        ..Default::default()
    };
    d.engine.storage().upsert_book(&book, None).await.unwrap()
}

async fn titles(d: &Device) -> Vec<String> {
    rows(d, "SELECT title FROM books ORDER BY title").await
}

fn is_off<T: std::fmt::Debug>(result: &crate::Result<T>) -> bool {
    matches!(result, Err(EngineError::Sync(SyncError::Off)))
}

/// Leaving keeps every row and needs no server; the device then syncs nothing, and a join
/// afterwards makes it a new device of the library, which syncs as any other.
#[tokio::test]
async fn a_device_that_leaves_keeps_its_library_and_joins_again_as_a_new_device() {
    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    add_edition(&a, "Middlemarch", "9780141439549").await;
    let code = create(&a, &server).await;
    join(&b, &server, &code).await;
    converge(&[&a, &b], &server).await;
    let identity = std::fs::read(&b.engine.config.sync_identity).unwrap();
    let (requests, devices) = (server.requests(), server.devices());

    b.engine.leave_sync_library().await.unwrap();
    assert_eq!(server.requests(), requests);
    assert_eq!(b.engine.sync_status().await.unwrap(), SyncStatus::Off);
    assert!(!b.engine.config.sync_identity.exists());
    assert_eq!(kernel_tables(&b).await, Vec::<String>::new());
    assert_eq!(titles(&b).await, ["Middlemarch"]);
    add_edition(&b, "Romola", "9780140434705").await;
    assert!(is_off(&sync_with(&b, &server, Receiver::default()).await));
    assert!(is_off(&b.engine.sync_recovery_code().await));
    b.engine.leave_sync_library().await.unwrap();
    assert_eq!(b.engine.sync_status().await.unwrap(), SyncStatus::Off);

    add_edition(&a, "Silas Marner", "9780141439754").await;
    converge(&[&a], &server).await;
    assert_eq!(titles(&a).await, ["Middlemarch", "Silas Marner"]);
    assert_eq!(titles(&b).await, ["Middlemarch", "Romola"]);

    join(&b, &server, &code).await;
    assert_ne!(
        std::fs::read(&b.engine.config.sync_identity).unwrap(),
        identity
    );
    assert_eq!(server.devices(), devices + 1);
    converge(&[&a, &b], &server).await;
    for d in [&a, &b] {
        assert_eq!(titles(d).await, ["Middlemarch", "Romola", "Silas Marner"]);
    }
}

/// A leave that stops after the identity is gone leaves a library that syncs nothing and says
/// so; leaving again finishes it. An identity left behind with no change capture, as the other
/// order would leave, is off, and leaving removes it.
#[tokio::test]
async fn a_leave_stopped_part_way_syncs_nothing_and_leaving_again_finishes_it() {
    let server = TestServer::default();
    let a = device().await;
    add_book(&a, "Middlemarch").await;
    create(&a, &server).await;
    sync(&a, &server).await;
    let identity = std::fs::read(&a.engine.config.sync_identity).unwrap();

    a.engine.sync.held.lock().await.fail_uninstall = true;
    assert!(a.engine.leave_sync_library().await.is_err());
    assert!(!a.engine.config.sync_identity.exists());
    assert!(!kernel_tables(&a).await.is_empty());

    let a = reopen(a).await;
    assert_eq!(
        a.engine.sync_status().await.unwrap(),
        SyncStatus::NeedsCredentials
    );
    let requests = server.requests();
    assert!(matches!(
        sync_with(&a, &server, Receiver::default()).await,
        Err(EngineError::Sync(SyncError::NeedsCredentials))
    ));
    assert_eq!(server.requests(), requests);
    a.engine.leave_sync_library().await.unwrap();
    assert_eq!(a.engine.sync_status().await.unwrap(), SyncStatus::Off);
    assert_eq!(kernel_tables(&a).await, Vec::<String>::new());

    std::fs::write(&a.engine.config.sync_identity, &identity).unwrap();
    let a = reopen(a).await;
    assert_eq!(a.engine.sync_status().await.unwrap(), SyncStatus::Off);
    a.engine.leave_sync_library().await.unwrap();
    assert!(!a.engine.config.sync_identity.exists());
    create(&a, &server).await;
    assert!(matches!(
        a.engine.sync_status().await.unwrap(),
        SyncStatus::On(_)
    ));
    assert_eq!(titles(&a).await, ["Middlemarch"]);
}

/// A leave waits for the sync running in its engine, and then removes what that sync used.
#[tokio::test]
async fn a_leave_waits_for_a_running_sync() {
    let server = TestServer::default();
    let a = device().await;
    create(&a, &server).await;
    add_book(&a, "Middlemarch").await;
    server.hold_at(1);
    let leaving = async {
        server.held().await;
        let leave = a.engine.leave_sync_library();
        tokio::pin!(leave);
        let early = tokio::time::timeout(std::time::Duration::from_millis(200), &mut leave).await;
        assert!(early.is_err(), "left while a sync ran");
        assert!(a.engine.config.sync_identity.exists());
        server.release();
        leave.await
    };
    let (synced, left) = tokio::join!(sync_with(&a, &server, Receiver::default()), leaving);
    synced.unwrap();
    left.unwrap();
    assert_eq!(a.engine.sync_status().await.unwrap(), SyncStatus::Off);
    assert_eq!(kernel_tables(&a).await, Vec::<String>::new());
    assert_eq!(titles(&a).await, ["Middlemarch"]);
}

/// Leaving while the rows sync was turned on over still wait to be adopted drops what waits and
/// keeps the rows; turning sync on again adopts them all.
#[tokio::test]
async fn a_leave_before_adopting_finished_keeps_every_row() {
    let server = TestServer::default();
    let (a, b) = (device().await, device().await);
    fill(&a.engine, 60, "Book", Isbns::None).await;
    {
        let mut held = a.engine.sync.held.lock().await;
        held.step_rows = Some(40);
        held.stop_after_steps = Some(2);
    }
    create(&a, &server).await;
    assert!(uncollected(&a).await > 0);
    a.engine.leave_sync_library().await.unwrap();
    assert_eq!(kernel_tables(&a).await, Vec::<String>::new());
    assert_eq!(scalar::<i64>(&a, "SELECT count(*) FROM books").await, 60);

    a.engine.sync.held.lock().await.stop_after_steps = None;
    let code = create(&a, &server).await;
    join(&b, &server, &code).await;
    converge(&[&a, &b], &server).await;
    assert_eq!(scalar::<i64>(&b, "SELECT count(*) FROM books").await, 60);
}

/// Another engine on the same data directory that is part-way through a sync when this one
/// leaves stops with sync off, and nothing it does turns change capture back on.
#[tokio::test]
async fn a_sync_in_another_engine_on_the_database_ends_off_when_this_one_leaves() {
    let server = TestServer::default();
    let a = device().await;
    create(&a, &server).await;
    add_book(&a, "Middlemarch").await;
    let other = Engine::open(EngineConfig::rooted_at(&a.root))
        .await
        .unwrap();
    server.hold_at(1);
    let leaving = async {
        server.held().await;
        let left = a.engine.leave_sync_library().await;
        server.release();
        left
    };
    let (synced, left) = tokio::join!(
        other.sync_now_with(|_| Ok(server.clone()), Receiver::default()),
        leaving
    );
    left.unwrap();
    assert!(is_off(&synced), "{synced:?}");
    assert_eq!(kernel_tables(&a).await, Vec::<String>::new());
    assert!(is_off(
        &other
            .sync_now_with(|_| Ok(server.clone()), Receiver::default())
            .await
    ));
    assert_eq!(other.sync_status().await.unwrap(), SyncStatus::Off);
    assert_eq!(titles(&a).await, ["Middlemarch"]);
}

/// A round of a sync that finds sync was left by another engine refuses, rather than install
/// change capture again.
#[tokio::test]
async fn a_round_after_another_engine_left_does_not_turn_sync_back_on() {
    let server = TestServer::default();
    let a = device().await;
    create(&a, &server).await;
    let other = Engine::open(EngineConfig::rooted_at(&a.root))
        .await
        .unwrap();
    other.leave_sync_library().await.unwrap();
    let mut conn = a.engine.storage().pool().acquire().await.unwrap();
    let refused =
        super::session::prepare(&mut conn, &super::replica().unwrap(), &Receiver::default())
            .await
            .unwrap_err();
    assert!(matches!(
        super::unwrap_application(refused),
        Ok(EngineError::Sync(SyncError::Off))
    ));
    drop(conn);
    assert_eq!(kernel_tables(&a).await, Vec::<String>::new());
}
