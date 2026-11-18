//! A device sync acts only on what is new from the device since its last report, so a put-down, a
//! pick-up or a reread made in the app stands until the device says something different.

mod common;

use std::path::PathBuf;

use bookmole::device::DeviceState;
use bookmole::{Engine, EngineConfig, HistorySpan, Reading, ReadingState};
use common::seed_book;

/// A file database, so a test can reach the rows through a second connection.
async fn engine() -> (tempfile::TempDir, Engine) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("database")).unwrap();
    let mut config = EngineConfig::in_memory(tmp.path());
    config.db_url = format!(
        "sqlite://{}?mode=rwc",
        tmp.path().join("database/app.db").display()
    );
    let engine = Engine::open(config).await.unwrap();
    (tmp, engine)
}

async fn backdate_readings(engine: &Engine) {
    let pool = sqlx::SqlitePool::connect(engine.db_url()).await.unwrap();
    sqlx::query("UPDATE readings SET last_modified = 1")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

const MD5: &str = "0123456789abcdef0123456789abcdef";

fn sidecar(status: Option<&str>, percent: Option<f64>) -> String {
    let percent = percent
        .map(|p| format!(r#"["percent_finished"] = {p},"#))
        .unwrap_or_default();
    let summary = status
        .map(|s| format!(r#"["summary"] = {{ ["status"] = "{s}" }},"#))
        .unwrap_or_default();
    format!(
        r#"
return {{
    ["annotations"] = {{
        [1] = {{
            ["datetime"] = "2026-01-05 21:14:08",
            ["pos0"] = "/body/DocFragment[1]/body/p[1]/text().0",
            ["pos1"] = "/body/DocFragment[1]/body/p[1]/text().9",
            ["text"] = "a passage",
        }},
    }},
    ["doc_props"] = {{ ["title"] = "Middlemarch", ["authors"] = "George Eliot" }},
    ["partial_md5_checksum"] = "{MD5}",
    {percent}
    {summary}
}}
"#
    )
}

struct Reader {
    dir: tempfile::TempDir,
}

impl Reader {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn sidecar_path(&self) -> PathBuf {
        self.dir.path().join("Middlemarch.sdr/metadata.epub.lua")
    }

    fn write(&self, status: Option<&str>, percent: impl Into<Option<f64>>) {
        let path = self.sidecar_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, sidecar(status, percent.into())).unwrap();
    }

    async fn sync(&self, engine: &Engine, status: Option<&str>, percent: impl Into<Option<f64>>) {
        self.write(status, percent);
        let report = engine
            .import_koreader(self.dir.path(), false)
            .await
            .unwrap();
        assert_eq!(report.imported.len(), 1, "the sidecar matched the book");
    }
}

async fn only_reading(engine: &Engine, book: i64) -> Reading {
    let readings = engine.list_readings(book).await.unwrap();
    assert_eq!(readings.len(), 1, "a sync never adds a reading here");
    readings.into_iter().next().unwrap()
}

async fn open_read(engine: &Engine, book: i64) -> Reading {
    let mut open: Vec<Reading> = engine
        .list_readings(book)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.finished_at.is_none())
        .collect();
    assert_eq!(open.len(), 1, "one open read");
    open.remove(0)
}

fn state_of(r: &Reading) -> ReadingState {
    ReadingState::from(r.status.as_str())
}

async fn state(engine: &Engine, book: i64) -> ReadingState {
    let r = only_reading(engine, book).await;
    assert_eq!(r.finished_at, None, "the read stays open");
    ReadingState::from(r.status.as_str())
}

async fn setup() -> (tempfile::TempDir, Engine, Reader, i64) {
    let (tmp, engine) = engine().await;
    let book = seed_book(&engine, "Middlemarch").await;
    (tmp, engine, Reader::new(), book)
}

#[tokio::test]
async fn a_pick_up_in_the_app_survives_a_resync_of_an_unchanged_abandoned_sidecar() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("abandoned"), 0.4).await;
    assert_eq!(state(&engine, book).await, ReadingState::Abandoned);

    engine.pick_up(book).await.unwrap();
    reader.sync(&engine, Some("abandoned"), 0.4).await;
    assert_eq!(state(&engine, book).await, ReadingState::Reading);
}

#[tokio::test]
async fn a_newly_abandoned_sidecar_puts_the_read_down() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("reading"), 0.4).await;
    assert_eq!(state(&engine, book).await, ReadingState::Reading);

    reader.sync(&engine, Some("abandoned"), 0.4).await;
    assert_eq!(state(&engine, book).await, ReadingState::Abandoned);
}

#[tokio::test]
async fn a_paused_read_stays_paused_through_an_unchanged_reading_sidecar() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("reading"), 0.4).await;
    engine.put_down(book).await.unwrap();

    reader.sync(&engine, Some("reading"), 0.4).await;
    assert_eq!(state(&engine, book).await, ReadingState::Abandoned);
}

#[tokio::test]
async fn reading_further_on_the_device_picks_a_paused_read_up() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("reading"), 0.4).await;
    engine.put_down(book).await.unwrap();
    assert!(engine.currently_reading(20).await.unwrap().is_empty());

    reader.sync(&engine, Some("reading"), 0.5).await;
    let r = only_reading(&engine, book).await;
    assert_eq!(ReadingState::from(r.status.as_str()), ReadingState::Reading);
    assert_eq!(r.put_down_at, None);
    assert_eq!(r.ko_percent, Some(0.5), "the device's position is taken");

    let open = engine.currently_reading(20).await.unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].0.id, Some(book));
    let timeline = engine
        .history_timeline(&HistorySpan::default())
        .await
        .unwrap();
    assert!(
        timeline.iter().any(|t| t.reading.id == r.id),
        "a picked-up read is back on the timeline"
    );
}

#[tokio::test]
async fn going_back_on_the_device_also_picks_a_paused_read_up() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("reading"), 0.4).await;
    engine.put_down(book).await.unwrap();

    reader.sync(&engine, Some("reading"), 0.3).await;
    assert_eq!(state(&engine, book).await, ReadingState::Reading);
}

#[tokio::test]
async fn moving_with_no_status_or_an_unknown_one_picks_a_paused_read_up() {
    for status in [None, Some("tbr")] {
        let (_tmp, engine, reader, book) = setup().await;
        reader.sync(&engine, status, 0.4).await;
        engine.put_down(book).await.unwrap();

        reader.sync(&engine, status, 0.4).await;
        assert_eq!(
            state(&engine, book).await,
            ReadingState::Abandoned,
            "{status:?} unchanged"
        );
        reader.sync(&engine, status, 0.6).await;
        assert_eq!(
            state(&engine, book).await,
            ReadingState::Reading,
            "{status:?} moved"
        );
    }
}

#[tokio::test]
async fn un_abandoning_on_the_device_picks_up_at_the_same_position() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("abandoned"), 0.4).await;
    assert_eq!(state(&engine, book).await, ReadingState::Abandoned);

    reader.sync(&engine, Some("reading"), 0.4).await;
    assert_eq!(state(&engine, book).await, ReadingState::Reading);
}

#[tokio::test]
async fn a_device_that_never_reported_on_the_read_leaves_it_paused() {
    let (_tmp, engine, reader, book) = setup().await;
    engine.update_progress(book, Some(80), None).await.unwrap();
    engine.put_down(book).await.unwrap();

    reader.sync(&engine, Some("reading"), 0.4).await;
    let r = only_reading(&engine, book).await;
    assert_eq!(
        ReadingState::from(r.status.as_str()),
        ReadingState::Abandoned
    );
    assert_eq!(r.ko_percent, Some(0.4), "the report is still recorded");
}

#[tokio::test]
async fn editing_the_page_while_paused_is_not_a_device_move() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("reading"), 0.4).await;
    engine.put_down(book).await.unwrap();
    engine.update_progress(book, Some(200), None).await.unwrap();
    assert_eq!(state(&engine, book).await, ReadingState::Abandoned);

    reader.sync(&engine, Some("reading"), 0.4).await;
    assert_eq!(state(&engine, book).await, ReadingState::Abandoned);
}

#[tokio::test]
async fn complete_on_the_device_finishes_a_paused_read() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("reading"), 0.4).await;
    engine.put_down(book).await.unwrap();

    reader.sync(&engine, Some("complete"), 1.0).await;
    let r = only_reading(&engine, book).await;
    assert!(r.finished_at.is_some());
    assert_eq!(
        ReadingState::from(r.status.as_str()),
        ReadingState::Finished
    );
}

#[tokio::test]
async fn a_second_identical_sync_writes_nothing() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("reading"), 0.4).await;
    engine.put_down(book).await.unwrap();
    reader.sync(&engine, Some("reading"), 0.5).await;
    assert_eq!(state(&engine, book).await, ReadingState::Reading);

    for status in ["reading", "abandoned", "complete"] {
        reader.sync(&engine, Some(status), 0.5).await;
        backdate_readings(&engine).await;
        let before = engine.list_readings(book).await.unwrap();
        reader.sync(&engine, Some(status), 0.5).await;
        assert_eq!(
            engine.list_readings(book).await.unwrap(),
            before,
            "a repeated {status} sidecar rewrote the reading"
        );
    }
}

#[tokio::test]
async fn a_report_without_a_percent_keeps_the_last_position() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("reading"), 0.4).await;
    engine.put_down(book).await.unwrap();

    reader.sync(&engine, Some("reading"), None).await;
    let r = only_reading(&engine, book).await;
    assert_eq!(state_of(&r), ReadingState::Abandoned);
    assert_eq!(r.ko_percent, Some(0.4), "no percent is not a position");

    reader.sync(&engine, Some("reading"), 0.6).await;
    assert_eq!(state(&engine, book).await, ReadingState::Reading);
}

#[tokio::test]
async fn a_reread_survives_an_unchanged_abandoned_sidecar() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("abandoned"), 0.4).await;
    engine.reread(book).await.unwrap();

    reader.sync(&engine, Some("abandoned"), 0.4).await;
    let r = open_read(&engine, book).await;
    assert_eq!(state_of(&r), ReadingState::Reading);
    assert_eq!(
        r.ko_status, None,
        "a stale report is not copied onto the new read"
    );
    assert_eq!(engine.list_readings(book).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_reread_survives_an_unchanged_complete_sidecar() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("complete"), 1.0).await;
    engine.reread(book).await.unwrap();

    reader.sync(&engine, Some("complete"), 1.0).await;
    let r = open_read(&engine, book).await;
    assert_eq!(state_of(&r), ReadingState::Reading);
    assert_eq!(r.ko_percent, None, "the new read does not show as all read");
}

#[tokio::test]
async fn after_a_reread_the_device_moves_the_new_read() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("complete"), 1.0).await;
    let first = only_reading(&engine, book).await;
    engine.reread(book).await.unwrap();

    reader.sync(&engine, Some("reading"), 0.2).await;
    let r = open_read(&engine, book).await;
    assert_eq!(state_of(&r), ReadingState::Reading);
    assert_eq!(r.ko_percent, Some(0.2));
    assert!(
        engine.list_readings(book).await.unwrap().contains(&first),
        "the closed read keeps its own report"
    );

    reader.sync(&engine, Some("complete"), 1.0).await;
    let readings = engine.list_readings(book).await.unwrap();
    assert_eq!(readings.len(), 2);
    assert!(
        readings.iter().all(|r| r.finished_at.is_some()),
        "a new complete finishes the new read"
    );
}

#[tokio::test]
async fn after_a_reread_a_newly_abandoned_sidecar_puts_the_new_read_down() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("abandoned"), 0.4).await;
    engine.reread(book).await.unwrap();

    reader.sync(&engine, Some("reading"), 0.1).await;
    assert_eq!(
        state_of(&open_read(&engine, book).await),
        ReadingState::Reading
    );
    reader.sync(&engine, Some("abandoned"), 0.1).await;
    assert_eq!(
        state_of(&open_read(&engine, book).await),
        ReadingState::Abandoned
    );
}

#[tokio::test]
async fn first_complete_finishes_and_repeats_change_nothing() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.sync(&engine, Some("complete"), 1.0).await;
    let finished = only_reading(&engine, book).await;
    assert!(finished.finished_at.is_some());
    reader.sync(&engine, Some("complete"), 1.0).await;
    assert_eq!(only_reading(&engine, book).await, finished);
}

/// A mount sync takes only the rows a scan calls syncable, so a status or position change alone must
/// make a row syncable.
#[tokio::test]
async fn a_mount_sync_takes_a_sidecar_whose_only_change_is_its_reading_state() {
    let (_tmp, engine, reader, book) = setup().await;
    reader.write(Some("reading"), 0.4);
    engine.sync_mount(reader.dir.path()).await.unwrap();
    engine.put_down(book).await.unwrap();

    let scan = engine.scan_device(reader.dir.path()).await.unwrap();
    assert_eq!(scan.books[0].state, DeviceState::Unchanged);
    engine.sync_mount(reader.dir.path()).await.unwrap();
    assert_eq!(state(&engine, book).await, ReadingState::Abandoned);

    reader.write(Some("reading"), 0.65);
    let scan = engine.scan_device(reader.dir.path()).await.unwrap();
    assert_eq!(
        scan.books[0].state,
        DeviceState::Updated {
            new_highlights: 0,
            refreshed: 0
        }
    );
    let synced = engine.sync_mount(reader.dir.path()).await.unwrap();
    assert_eq!(synced.synced, 1);
    assert_eq!(state(&engine, book).await, ReadingState::Reading);

    reader.write(Some("abandoned"), 0.65);
    engine.sync_mount(reader.dir.path()).await.unwrap();
    assert_eq!(state(&engine, book).await, ReadingState::Abandoned);

    let scan = engine.scan_device(reader.dir.path()).await.unwrap();
    assert_eq!(scan.books[0].state, DeviceState::Unchanged);
}

#[tokio::test]
async fn a_single_sidecar_pull_shares_the_rule() {
    let (_tmp, engine, reader, _) = setup().await;
    reader.write(Some("abandoned"), 0.4);
    let first = engine
        .pull_book_from_sidecar(&reader.sidecar_path())
        .await
        .unwrap();
    let book = first.stats.book_id;
    assert_eq!(state(&engine, book).await, ReadingState::Abandoned);

    engine.pick_up(book).await.unwrap();
    engine
        .pull_book_from_sidecar(&reader.sidecar_path())
        .await
        .unwrap();
    assert_eq!(state(&engine, book).await, ReadingState::Reading);

    reader.write(Some("reading"), 0.4);
    engine.put_down(book).await.unwrap();
    engine
        .pull_book_from_sidecar(&reader.sidecar_path())
        .await
        .unwrap();
    assert_eq!(
        state(&engine, book).await,
        ReadingState::Reading,
        "abandoned to reading on the device picks it up"
    );
}
