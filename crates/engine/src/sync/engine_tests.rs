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
    let images = d.root.join("database/images");
    std::fs::create_dir_all(&images).unwrap();
    let file = images.join(format!("{}.jpg", format!("{:02x}", book as u8).repeat(32)));
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
    let (first, second) = tokio::join!(sync(&a, &server), sync(&a, &server));
    assert!(first.sent != second.sent, "{first:?} {second:?}");
    assert_eq!(server.most_in_flight(), 1);
    // Each sync asks for the capabilities first; the second may ask only once the first has
    // appended what it had, and nothing is appended twice.
    let seen = &server.endpoints()[before..];
    let at = |what| {
        seen.iter()
            .enumerate()
            .filter(move |(_, e)| **e == what)
            .map(|(i, _)| i)
    };
    let asked: Vec<usize> = at("capabilities").collect();
    let appended: Vec<usize> = at("append").collect();
    assert_eq!(asked.len(), 2, "{seen:?}");
    assert_eq!(appended.len(), 1, "{seen:?}");
    assert!(asked[0] < appended[0] && appended[0] < asked[1], "{seen:?}");
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

/// Writes through the engine's pool, which waits up to five seconds for the lock, and says how
/// long that took.
async fn pool_write(d: &Device) -> std::time::Duration {
    let started = std::time::Instant::now();
    add_book(d, "Felix Holt").await;
    sqlx::query("DELETE FROM books WHERE title = 'Felix Holt'")
        .execute(d.engine.storage().pool())
        .await
        .unwrap();
    started.elapsed()
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
    let prompt = std::time::Duration::from_secs(2);

    let mut hanging = Receiver::default();
    hanging.faults.hang = true;
    let dropped = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        sync_with(&b, &server, hanging),
    )
    .await;
    assert!(dropped.is_err(), "the apply did not hang");
    assert!(pool_write(&b).await < prompt);

    let mut panicking = Receiver::default();
    panicking.faults.panic = true;
    let caught = std::panic::AssertUnwindSafe(sync_with(&b, &server, panicking))
        .catch_unwind()
        .await;
    assert!(caught.is_err(), "the apply did not panic");
    assert!(pool_write(&b).await < prompt);

    for n in 1..=8 {
        server.hang_at(Some(n));
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(300),
            sync_with(&b, &server, Receiver::default()),
        )
        .await;
        server.hang_at(None);
        assert!(pool_write(&b).await < prompt, "dropped at request {n}");
    }

    let opened = b.engine.sync.held.lock().await.opened;
    sync(&b, &server).await;
    assert!(b.engine.sync.held.lock().await.opened > opened);
    assert_eq!(
        rows(&b, "SELECT title FROM books ORDER BY title").await,
        ["Adam Bede", "Middlemarch", "Romola"]
    );
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
        cover_file(&b, book(&b, "Middlemarch").await).await,
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

    let refused = b
        .engine
        .storage()
        .insert_note(
            NewNoteMeta {
                book_id: None,
                reading_id: None,
                highlight_id: None,
                page: None,
                location: None,
                file_path: "../x.md",
                title: "Prelude",
                kind: "note",
            },
            "",
            &[],
        )
        .await;
    assert!(matches!(refused, Err(EngineError::InvalidInput(_))));

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
