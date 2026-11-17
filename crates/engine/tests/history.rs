//! Reading history, figures, records, CSV export and the settings store, over a seeded year
//! of reading in a file database.

mod common;

use bookmole::{
    Book, Confidence, Engine, EngineConfig, HistoryCompare, HistoryGroup, HistoryMetric,
    HistorySpan, HistorySplit, NewNoteInput, NewReadingEvent, NoteKind, ReadingState, SeriesQuery,
};

async fn engine() -> (tempfile::TempDir, Engine) {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("database")).unwrap();
    let config = EngineConfig {
        db_url: format!(
            "sqlite://{}?mode=rwc",
            tmp.path().join("database/app.db").display()
        ),
        images_dir: tmp.path().join("database/images"),
        files_dir: tmp.path().join("database/files"),
        vault_dir: tmp.path().join("vault"),
        log_dir: tmp.path().join("logs"),
        exports_dir: tmp.path().join("exports"),
        google_api_key: None,
        calibre_bin_dir: None,
    };
    let engine = Engine::open(config).await.expect("engine opens");
    (tmp, engine)
}

async fn raw(engine: &Engine, sql: &str, a: i64, b: i64) {
    let pool = sqlx::SqlitePool::connect(engine.db_url()).await.unwrap();
    sqlx::query(sql)
        .bind(a)
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
}

fn at(day: &str) -> i64 {
    bookmole::history::midnight(bookmole::history::parse_day(day).unwrap()) + 12 * 3600
}

fn year(y: i32) -> HistorySpan {
    let start = |y: i32| at(&format!("{y}-01-01")) - 12 * 3600;
    HistorySpan::new(Some(start(y)), Some(start(y + 1))).unwrap()
}

async fn book(engine: &Engine, title: &str, author: &str, pages: i64) -> i64 {
    engine
        .save_book(&Book {
            title: Some(title.into()),
            authors: vec![author.into()],
            page_count: Some(pages),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap()
}

async fn read(engine: &Engine, book: i64, start: &str, end: Option<&str>) -> i64 {
    engine
        .storage()
        .record_reading(
            book,
            Some(at(start)),
            end.map(at),
            if end.is_some() { "finished" } else { "reading" },
            "manual",
        )
        .await
        .unwrap()
}

async fn rate(engine: &Engine, book: i64, reading: i64, value: f64, day: &str) {
    let review = engine.open_review(book, Some(reading)).await.unwrap();
    engine.set_rating(review.id, value).await.unwrap();
    raw(
        engine,
        "UPDATE notes SET created_at = ?1, latest_part_at = ?1 WHERE id = ?2",
        at(day),
        review.id,
    )
    .await;
}

async fn passage(engine: &Engine, book: i64, text: &str, day: &str) {
    engine
        .storage()
        .insert_highlight(book, &common::highlight(text, &format!("{day} 09:00:00")))
        .await
        .unwrap();
    engine.storage().attribute_highlights(book).await.unwrap();
}

async fn note(engine: &Engine, book: i64, body: &str, day: &str) -> i64 {
    let id = engine
        .create_note(NewNoteInput {
            book_id: Some(book),
            kind: NoteKind::Note,
            body: body.into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .id;
    raw(
        engine,
        "UPDATE notes SET created_at = ?1, latest_part_at = ?1 WHERE id = ?2",
        at(day),
        id,
    )
    .await;
    id
}

async fn pages(engine: &Engine, book: i64, day: &str, pages: i64, minutes: Option<i64>) {
    engine
        .storage()
        .record_reading_events(&[NewReadingEvent {
            book_id: book,
            reading_id: None,
            day: day.into(),
            minutes,
            pages: Some(pages),
            source: "koreader".into(),
            confidence: Confidence::Measured,
        }])
        .await
        .unwrap();
}

struct Shelf {
    winter_king: i64,
    long_country: i64,
    arithmetic: i64,
    orchard: i64,
    hours: i64,
    unread: i64,
    inheritance: i64,
    wk_read: i64,
    lc_read: i64,
    ar_read: i64,
    or_read: i64,
}

async fn shelf(engine: &Engine) -> Shelf {
    let winter_king = book(engine, "Winter King", "Ruth Lindqvist", 618).await;
    let long_country = book(engine, "The Long Country", "Owen Dziedzic", 526).await;
    let arithmetic = book(engine, "A Brief Arithmetic", "Yuki Dziedzic", 232).await;
    let orchard = book(engine, "The Pale Orchard", "Nadia Grigorescu", 356).await;
    let hours = book(engine, "Winter Hours", "Tobias Eshun", 280).await;
    let unread = book(engine, "Salt Machine", "Owen Dziedzic", 264).await;
    let inheritance = book(engine, "Ordinary Inheritance", "Yuki Dziedzic", 232).await;

    let wk_read = read(engine, winter_king, "2026-01-05", Some("2026-03-27")).await;
    let lc_read = read(engine, long_country, "2026-05-14", Some("2026-07-08")).await;
    let ar_read = read(engine, arithmetic, "2026-07-30", Some("2026-08-28")).await;
    let or_read = read(engine, orchard, "2026-08-20", None).await;
    engine.storage().abandon_reading(orchard).await.unwrap();
    raw(
        engine,
        "UPDATE readings SET put_down_at = ?1 WHERE id = ?2",
        at("2026-09-12"),
        or_read,
    )
    .await;
    read(engine, hours, "2026-10-20", None).await;
    read(engine, inheritance, "2025-11-18", Some("2025-12-20")).await;

    rate(engine, winter_king, wk_read, 4.0, "2026-03-27").await;
    rate(engine, long_country, lc_read, 5.0, "2026-07-08").await;
    rate(engine, arithmetic, ar_read, 5.0, "2026-08-28").await;

    for (i, d) in ["2026-01-20", "2026-02-02", "2026-03-01"]
        .iter()
        .enumerate()
    {
        passage(engine, winter_king, &format!("wk{i}"), d).await;
    }
    passage(engine, arithmetic, "ar0", "2026-08-01").await;
    note(engine, winter_king, "a thought while reading", "2026-02-10").await;
    note(engine, winter_king, "a thought afterwards", "2026-04-15").await;

    pages(engine, winter_king, "2026-01-05", 40, Some(60)).await;
    pages(engine, winter_king, "2026-01-06", 30, None).await;
    pages(engine, long_country, "2026-06-01", 50, Some(45)).await;
    pages(engine, inheritance, "2025-06-01", 25, None).await;
    pages(engine, inheritance, "2025-06-02", 10, None).await;
    pages(engine, inheritance, "2025-06-03", 5, None).await;

    Shelf {
        winter_king,
        long_country,
        arithmetic,
        orchard,
        hours,
        unread,
        inheritance,
        wk_read,
        lc_read,
        ar_read,
        or_read,
    }
}

#[tokio::test]
async fn the_timeline_holds_the_years_reads_newest_end_first() {
    let (_tmp, engine) = engine().await;
    let s = shelf(&engine).await;
    let reads = engine.history_timeline(&year(2026)).await.unwrap();
    let books: Vec<i64> = reads.iter().map(|r| r.reading.book_id).collect();
    assert_eq!(
        books,
        vec![
            s.hours,
            s.orchard,
            s.arithmetic,
            s.long_country,
            s.winter_king
        ],
        "the open read first, then each by where it ended; 2025's read and the unread book are not in 2026"
    );
    assert!(!books.contains(&s.unread) && !books.contains(&s.inheritance));

    let orchard = &reads[1];
    assert_eq!(orchard.state, ReadingState::Abandoned);
    assert_eq!(orchard.put_down_at, Some(at("2026-09-12")));
    assert_eq!(orchard.days, Some(23), "put down 12 Sep, started 20 Aug");

    let wk = reads.iter().find(|r| r.reading.id == s.wk_read).unwrap();
    assert_eq!(wk.months, vec!["2026-01", "2026-02", "2026-03"]);
    assert_eq!(wk.passages, 3);
    assert_eq!(
        wk.notes, 2,
        "the review lives on the read, and the ordinary note written during it is placed by its window; the one after is not"
    );
    assert_eq!(wk.rating.as_ref().map(|r| r.value), Some(4.0));
    assert_eq!(wk.days, Some(81));

    let only_2025 = engine.history_timeline(&year(2025)).await.unwrap();
    assert_eq!(only_2025.len(), 1);
    assert_eq!(only_2025[0].reading.book_id, s.inheritance);
}

#[tokio::test]
async fn reading_now_is_the_open_reads_whatever_the_period() {
    let (_tmp, engine) = engine().await;
    let s = shelf(&engine).await;
    let early = book(&engine, "An Early Start", "Ruth Lindqvist", 300).await;
    read(&engine, early, "2025-12-01", None).await;
    passage(&engine, early, "early0", "2025-12-02").await;

    let now = engine.history_reading_now().await.unwrap();
    let books: Vec<i64> = now.iter().map(|r| r.reading.book_id).collect();
    assert_eq!(
        books,
        vec![s.hours, early],
        "the open reads, most recently begun first; the put-down read and the finished ones are not reading"
    );
    assert!(now.iter().all(|r| r.state == ReadingState::Reading));
    assert!(now.iter().all(|r| r.ended_at.is_none()));
    assert_eq!(now[1].passages, 1, "this read's passages ride along");

    let all = engine
        .history_timeline(&HistorySpan::default())
        .await
        .unwrap();
    for r in &now {
        let t = all.iter().find(|t| t.reading.id == r.reading.id).unwrap();
        assert_eq!((t.passages, t.notes), (r.passages, r.notes));
    }
}

#[tokio::test]
async fn the_overview_figures_count_the_span_and_never_a_current_run() {
    let (_tmp, engine) = engine().await;
    let s = shelf(&engine).await;
    let f = engine.history_figures(&year(2026)).await.unwrap();
    assert_eq!(f.books_finished, 3);
    assert_eq!(f.put_down, 1);
    assert_eq!(
        f.reading_now, 1,
        "Winter Hours; the put-down read is not under way"
    );
    assert_eq!(f.pages, Some(120));
    assert_eq!(f.minutes, Some(105));
    assert_eq!(
        f.minutes_from
            .iter()
            .map(|m| m.key.as_str())
            .collect::<Vec<_>>(),
        vec!["koreader"]
    );
    assert_eq!(f.passages, 4);
    assert_eq!(
        f.passages_per_finished_book,
        Some(4.0 / 3.0),
        "three finished reads holding four passages between them"
    );
    assert_eq!((f.notes, f.reviews, f.reflections), (5, 3, 0));

    let y25 = engine.history_figures(&year(2025)).await.unwrap();
    assert_eq!(y25.elapsed_days, 365);
    assert_eq!(y25.pages, Some(40));
    assert!((y25.pages_per_day.unwrap() - 40.0 / 365.0).abs() < 1e-9);
    let run = y25
        .longest_run
        .expect("1–3 June 2025 is a run that has ended");
    assert_eq!(
        (run.days, run.first.as_str(), run.last.as_str()),
        (3, "2025-06-01", "2025-06-03")
    );
    let _ = s;
}

#[tokio::test]
async fn the_records_name_their_reads_and_ties_go_to_the_latest_finish() {
    let (_tmp, engine) = engine().await;
    let s = shelf(&engine).await;
    let r = engine.history_records(&year(2026)).await.unwrap();
    let fastest = r.fastest.unwrap();
    assert_eq!(fastest.read.reading.id, s.ar_read);
    assert_eq!(fastest.read.days, Some(29));
    assert!((fastest.pages_per_day.unwrap() - 232.0 / 29.0).abs() < 1e-9);
    let longest = r.longest.unwrap();
    assert_eq!(
        (longest.read.reading.id, longest.pages),
        (s.wk_read, Some(618))
    );
    assert_eq!(r.most_marked.unwrap().read.reading.id, s.wk_read);
    let best = r.best_rated.unwrap();
    assert_eq!(
        best.read.reading.id, s.ar_read,
        "5 and 5: the later finish wins"
    );
    assert_eq!(
        best.tied_with
            .iter()
            .map(|t| t.reading.id)
            .collect::<Vec<_>>(),
        vec![s.lc_read]
    );
    let _ = (s.or_read, s.long_country);
}

#[tokio::test]
async fn a_series_sums_splits_and_compares_with_last_year() {
    let (_tmp, engine) = engine().await;
    let s = shelf(&engine).await;
    let q = SeriesQuery {
        metric: HistoryMetric::Pages,
        group: HistoryGroup::Week,
        split: HistorySplit::Book,
        compare: HistoryCompare::LastYear,
        span: year(2026),
    };
    let series = engine.history_series(&q).await.unwrap();
    assert_eq!(series.total, 120);
    assert_eq!(series.buckets.iter().map(|b| b.value).sum::<i64>(), 120);
    assert_eq!(series.buckets[0].start, "2026-01-01");
    assert_eq!(
        series.buckets[0].period, "2025-12-29",
        "weeks start on Monday"
    );
    let first_week = &series.buckets[1];
    assert_eq!(first_week.period, "2026-01-05");
    assert_eq!(first_week.value, 70);
    assert_eq!(first_week.parts.len(), 1);
    assert_eq!(first_week.parts[0].key, s.winter_king.to_string());
    assert_eq!(first_week.parts[0].label, "Winter King");
    assert!(
        series
            .buckets
            .iter()
            .all(|b| { b.parts.iter().map(|p| p.value).sum::<i64>() == b.value })
    );
    assert!(
        !series.buckets[2].known,
        "a week nothing measured is no data, not zero"
    );
    assert_eq!(
        series.compare_total,
        Some(40),
        "June 2025, 52 weeks earlier"
    );
    assert!((series.compare_change.unwrap() - 2.0).abs() < 1e-9);
    let busiest = series.busiest.unwrap();
    assert_eq!(series.buckets[busiest].period, "2026-01-05");

    let average = engine
        .history_series(&SeriesQuery {
            compare: HistoryCompare::Average,
            split: HistorySplit::Unsplit,
            ..q
        })
        .await
        .unwrap();
    assert!(average.buckets.iter().all(|b| b.compare == average.average));
    assert!(average.buckets.iter().all(|b| b.parts.is_empty()));

    let finished = engine
        .history_series(&SeriesQuery {
            metric: HistoryMetric::BooksFinished,
            group: HistoryGroup::Month,
            split: HistorySplit::Unsplit,
            compare: HistoryCompare::Nothing,
            span: year(2026),
        })
        .await
        .unwrap();
    assert_eq!(finished.total, 3);
    assert!(finished.known);
    assert_eq!(finished.buckets[0].period, "2026-01-01");

    let path = engine.export_history_series(&q).await.unwrap();
    assert!(path.path.starts_with(engine.exports_dir()));
    let text = std::fs::read_to_string(&path.path).unwrap();
    assert!(text.starts_with("start,end,period,part,label,value,compare\n"));
    assert_eq!(text.lines().count() as i64, path.rows + 1);
}

#[tokio::test]
async fn what_was_kept_is_by_book_most_first() {
    let (_tmp, engine) = engine().await;
    let s = shelf(&engine).await;
    let kept = engine.history_kept(&year(2026), -1).await.unwrap();
    let ids: Vec<(i64, i64, i64)> = kept
        .iter()
        .map(|k| (k.book.id.unwrap(), k.passages, k.notes))
        .collect();
    assert_eq!(
        ids[0],
        (s.winter_king, 3, 3),
        "three passages, two notes and a review"
    );
    assert_eq!(engine.history_kept(&year(2026), 1).await.unwrap().len(), 1);
    assert!(
        engine
            .history_kept(&year(2024), -1)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_inverted_span_is_refused() {
    assert!(HistorySpan::new(Some(10), Some(5)).is_err());
}

#[tokio::test]
async fn settings_round_trip_through_the_facade() {
    let (_tmp, engine) = engine().await;
    engine
        .set_setting("history.chart.pages", r#"{"group":"week"}"#)
        .await
        .unwrap();
    assert_eq!(
        engine
            .get_setting("history.chart.pages")
            .await
            .unwrap()
            .as_deref(),
        Some(r#"{"group":"week"}"#)
    );
    assert_eq!(engine.list_settings("history.").await.unwrap().len(), 1);
    assert!(
        engine
            .set_setting("history.chart.pages", "{")
            .await
            .is_err()
    );
}
