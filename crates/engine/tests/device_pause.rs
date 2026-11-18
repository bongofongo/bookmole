//! A device sync acts only on what is new from the device since its last report, so a put-down or a
//! pick-up made in the app stands until the device says something different.

mod common;

use std::path::PathBuf;

use bookmole::{Engine, HistorySpan, Reading, ReadingState};
use common::{engine, seed_book};

const MD5: &str = "0123456789abcdef0123456789abcdef";

fn sidecar(status: Option<&str>, percent: f64) -> String {
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
    ["percent_finished"] = {percent},
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

    fn write(&self, status: Option<&str>, percent: f64) {
        let path = self.sidecar_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, sidecar(status, percent)).unwrap();
    }

    async fn sync(&self, engine: &Engine, status: Option<&str>, percent: f64) {
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
    let picked_up = only_reading(&engine, book).await;

    reader.sync(&engine, Some("reading"), 0.5).await;
    assert_eq!(only_reading(&engine, book).await, picked_up);

    reader.sync(&engine, Some("abandoned"), 0.5).await;
    let put_down = only_reading(&engine, book).await;
    reader.sync(&engine, Some("abandoned"), 0.5).await;
    assert_eq!(only_reading(&engine, book).await, put_down);
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
