//! Reading time from the device's `statistics.sqlite3`, joined on `book.md5`, as one more writer of
//! `reading_events`. Its UTC days can differ from the sidecars' local-as-UTC days, by design.
//! Durations, page counts and days are private reading: never logged above `trace!`.

use std::path::{Path, PathBuf};

use sqlx::{Connection, Row, SqliteConnection, sqlite::SqliteConnectOptions};

use crate::device::koreader_dir;
use crate::diagnostic::Diagnostic;
use crate::error::{EngineError, Result};
use crate::storage::{Confidence, FillStats, NewReadingEvent, SOURCE_KOREADER, Storage};

/// `DB_SCHEMA_VERSION` stamped into `PRAGMA user_version` by the plugin. Any other version is
/// refused: a wrong minutes figure is indistinguishable downstream and would set `confidence` to
/// `measured`.
pub const KNOWN_SCHEMA_VERSION: i64 = 20221111;

pub const STATS_DB_NAME: &str = "statistics.sqlite3";
const SETTINGS_DIR: &str = "settings";

/// Counts only; `minutes` is not summarised here, that belongs to [`Storage::activity_summary`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsImportReport {
    pub schema_version: Option<i64>,
    pub books_in_db: usize,
    pub books_matched: usize,
    pub days: usize,
    pub events: FillStats,
    pub warnings: Vec<Diagnostic>,
}

pub fn statistics_db(ko_dir: &Path) -> PathBuf {
    ko_dir.join(SETTINGS_DIR).join(STATS_DB_NAME)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MeasuredDay {
    md5: String,
    day: String,
    seconds: i64,
    pages: i64,
}

/// Reads `page_stat_data`, not the `page_stat` view (its rescaling loses seconds and inflates page
/// counts). `start_time > 0` drops the default 0, which would land on 1970-01-01. `date()` is NULL
/// for unreadable times and `HAVING` drops those, so no source invents a day.
const AGGREGATE: &str = "
    SELECT b.md5                            AS md5,
           date(p.start_time, 'unixepoch')  AS day,
           sum(p.duration)                  AS seconds,
           count(DISTINCT p.page)           AS pages
      FROM page_stat_data p
      JOIN book b ON b.id = p.id_book
     WHERE b.md5 IS NOT NULL AND b.md5 <> '' AND p.start_time > 0
     GROUP BY b.md5, day
    HAVING day IS NOT NULL
     ORDER BY b.md5, day";

/// Not part of `sync_device`'s default path: plugging a device in reads and writes nothing, so the
/// user asks for this by name.
pub async fn import_device_statistics(
    storage: &Storage,
    mount: &Path,
) -> Result<StatsImportReport> {
    let Some(ko_dir) = koreader_dir(mount) else {
        return Err(EngineError::InvalidInput(format!(
            "{} is not a KOReader device",
            mount.display()
        )));
    };
    import_statistics(storage, &statistics_db(&ko_dir)).await
}

/// Never fails on absence: a missing or unreadable database, unknown schema or unheld book is a
/// [`Diagnostic`] and an empty report. Unmeasured days get no row, so minutes are absent, not zero.
#[tracing::instrument(skip(storage), fields(path = %db_path.display()))]
pub async fn import_statistics(storage: &Storage, db_path: &Path) -> Result<StatsImportReport> {
    let mut report = StatsImportReport::default();

    if !db_path.is_file() {
        report
            .warnings
            .push(Diagnostic::statistics_db_absent(db_path.to_path_buf()));
        return Ok(report);
    }

    // Copy first: the device's database may be mid-write, so SQLite never opens the original.
    let scratch = tempfile::tempdir()?;
    let copy = match copy_out(db_path, scratch.path()) {
        Ok(c) => c,
        Err(e) => {
            report.warnings.push(Diagnostic::statistics_db_unreadable(
                db_path.to_path_buf(),
                &EngineError::from(e),
            ));
            return Ok(report);
        }
    };

    // Every read of the copy is inside this one fallible region: SQLite does not notice a non-database
    // at open, only on the first statement, so a read outside it would leak a raw sqlx error.
    let measured = match read_statistics(&copy, db_path, &mut report).await {
        Ok(Some(days)) => days,
        Ok(None) => return Ok(report),
        Err(e) => {
            report.warnings.push(Diagnostic::statistics_db_unreadable(
                db_path.to_path_buf(),
                &e,
            ));
            return Ok(report);
        }
    };

    write_events(storage, measured, &mut report).await?;

    tracing::debug!(
        books_in_db = report.books_in_db,
        books_matched = report.books_matched,
        days = report.days,
        inserted = report.events.inserted,
        updated = report.events.updated,
        "imported device statistics"
    );
    Ok(report)
}

/// `Ok(None)` is the version gate refusing (diagnostic already recorded); `Err` means SQLite would
/// not read the file. The split keeps "chose not to" apart from "could not".
async fn read_statistics(
    copy: &Path,
    origin: &Path,
    report: &mut StatsImportReport,
) -> Result<Option<Vec<MeasuredDay>>> {
    let mut conn = open_copy(copy).await?;

    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut conn)
        .await?;
    report.schema_version = Some(version);
    if version != KNOWN_SCHEMA_VERSION {
        report.warnings.push(Diagnostic::statistics_schema_unknown(
            origin.to_path_buf(),
            version,
        ));
        conn.close().await.ok();
        return Ok(None);
    }

    let days = read_days(&mut conn, report).await?;
    conn.close().await.ok();
    Ok(Some(days))
}

/// The book list is walked apart from the aggregate so a book with no usable `md5` and a book we do
/// not hold are each reported once.
async fn read_days(
    conn: &mut SqliteConnection,
    report: &mut StatsImportReport,
) -> Result<Vec<MeasuredDay>> {
    let books = sqlx::query("SELECT title, md5 FROM book")
        .fetch_all(&mut *conn)
        .await?;
    report.books_in_db = books.len();
    for b in &books {
        let md5: Option<String> = b.get("md5");
        if md5.as_deref().unwrap_or("").is_empty() {
            let title: Option<String> = b.get("title");
            report
                .warnings
                .push(Diagnostic::statistics_book_not_identified(
                    title.as_deref().unwrap_or("(untitled)"),
                ));
        }
    }

    let rows = sqlx::query(AGGREGATE).fetch_all(&mut *conn).await?;
    Ok(rows
        .iter()
        .map(|r| MeasuredDay {
            md5: r.get("md5"),
            day: r.get("day"),
            seconds: r.get("seconds"),
            pages: r.get("pages"),
        })
        .collect())
}

/// One `record_reading_events` call, so no-clobber merge and idempotency are `reading_events`' own.
async fn write_events(
    storage: &Storage,
    measured: Vec<MeasuredDay>,
    report: &mut StatsImportReport,
) -> Result<()> {
    let mut events = Vec::new();
    let mut unmatched = std::collections::BTreeSet::new();
    let mut matched = std::collections::BTreeSet::new();
    // Rows arrive grouped by md5, so the join is resolved once per book, not once per row.
    let mut seen: std::collections::HashMap<String, Option<i64>> = std::collections::HashMap::new();

    for m in measured {
        let book_id = match seen.get(&m.md5) {
            Some(hit) => *hit,
            None => {
                let id = storage
                    .find_book_by_partial_md5(&m.md5)
                    .await?
                    .and_then(|b| b.id);
                seen.insert(m.md5.clone(), id);
                id
            }
        };
        let Some(book_id) = book_id else {
            unmatched.insert(m.md5);
            continue;
        };
        matched.insert(book_id);

        // Per-file statistics know nothing of rereads: attribution comes from our reading windows, else
        // NULL.
        let reading_id = storage.reading_for_day(book_id, &m.day).await?;

        tracing::trace!(
            book_id,
            day = %m.day,
            seconds = m.seconds,
            pages = m.pages,
            "measured day"
        );

        events.push(NewReadingEvent {
            book_id,
            reading_id,
            day: m.day,
            minutes: Some(minutes_of(m.seconds)),
            pages: Some(m.pages),
            source: SOURCE_KOREADER.to_string(),
            confidence: Confidence::Measured,
        });
    }

    for md5 in unmatched {
        report
            .warnings
            .push(Diagnostic::statistics_book_unmatched(&md5));
    }
    report.books_matched = matched.len();
    report.days = events.len();
    report.events = storage.record_reading_events(&events).await?;
    Ok(())
}

/// Seconds to whole minutes, rounded to nearest (floor would bias every day down). A short but real
/// day gives `Some(0)`, not `None`: `None` is only for days the device never measured.
fn minutes_of(seconds: i64) -> i64 {
    (seconds.max(0) + 30) / 60
}

/// Copies the database and its `-wal` and `-shm` siblings into `dir`; the `-wal` is required (the
/// main file alone is the last checkpoint). Keeps the file name so sibling names still match.
pub(crate) fn copy_out(src: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    let name = src
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new(STATS_DB_NAME));
    let dst = dir.join(name);
    std::fs::copy(src, &dst)?;
    for suffix in ["-wal", "-shm"] {
        let sibling = with_suffix(src, suffix);
        if sibling.is_file() {
            std::fs::copy(&sibling, with_suffix(&dst, suffix))?;
        }
    }
    Ok(dst)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// `create_if_missing(false)` is load-bearing: a typo'd path must not become an empty database.
/// Opened read-write because SQLite must replay the copied `-wal`; read-only would skip recovery.
async fn open_copy(path: &Path) -> Result<SqliteConnection> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false);
    Ok(SqliteConnection::connect_with(&opts).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_round_to_the_nearest_minute_and_a_short_day_is_measured_zero() {
        assert_eq!(minutes_of(0), 0);
        assert_eq!(minutes_of(29), 0, "a genuinely tiny day is a measured zero");
        assert_eq!(minutes_of(30), 1);
        assert_eq!(minutes_of(90), 2, "nearest, not floor");
        assert_eq!(minutes_of(3600), 60);
        assert_eq!(minutes_of(-5), 0);
    }

    #[test]
    fn the_wal_siblings_are_named_the_way_sqlite_names_them() {
        let p = Path::new("/x/statistics.sqlite3");
        assert_eq!(
            with_suffix(p, "-wal"),
            Path::new("/x/statistics.sqlite3-wal"),
            "SQLite appends to the whole name; it does not replace the extension"
        );
        assert_eq!(
            with_suffix(p, "-shm"),
            Path::new("/x/statistics.sqlite3-shm")
        );
    }

    #[test]
    fn the_db_sits_in_the_settings_dir_of_the_install() {
        assert_eq!(
            statistics_db(Path::new("/Volumes/KOBOeReader/.adds/koreader")),
            Path::new("/Volumes/KOBOeReader/.adds/koreader/settings/statistics.sqlite3")
        );
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::book::Book;
    use crate::storage::{DayRange, LinkedBy, day_of_unix};
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    const BASE: i64 = 1_767_571_200;
    const DAY: i64 = 86_400;

    const DDL: &str = "
        PRAGMA user_version=20221111;
        CREATE TABLE book (id integer PRIMARY KEY autoincrement, title text,
            authors text, notes integer, last_open integer, highlights integer,
            pages integer, series text, language text, md5 text,
            total_read_time integer, total_read_pages integer);
        CREATE TABLE page_stat_data (id_book integer, page integer NOT NULL DEFAULT 0,
            start_time integer NOT NULL DEFAULT 0, duration integer NOT NULL DEFAULT 0,
            total_pages integer NOT NULL DEFAULT 0,
            UNIQUE (id_book, page, start_time), FOREIGN KEY(id_book) REFERENCES book(id));";

    #[derive(Debug, Clone)]
    struct Session {
        book: usize,
        day: u8,
        seconds: u16,
    }

    fn session() -> impl Strategy<Value = Session> {
        // `seconds` reaches below 30 on purpose: a sub-minute session separates "measured zero" from
        // "absent".
        (0usize..3, 0u8..20, 0u16..2400).prop_map(|(book, day, seconds)| Session {
            book,
            day,
            seconds,
        })
    }

    fn md5_of(book: usize) -> String {
        format!("{book:032x}")
    }

    async fn device(dir: &Path, sessions: &[Session]) -> PathBuf {
        let path = dir.join(STATS_DB_NAME);
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let mut conn = SqliteConnection::connect_with(&opts).await.unwrap();
        sqlx::raw_sql(DDL).execute(&mut conn).await.unwrap();
        for book in 0..3 {
            sqlx::query("INSERT INTO book (id, title, md5) VALUES (?, ?, ?)")
                .bind(book as i64 + 1)
                .bind(format!("book {book}"))
                .bind(md5_of(book))
                .execute(&mut conn)
                .await
                .unwrap();
        }
        for (n, s) in sessions.iter().enumerate() {
            sqlx::query(
                "INSERT INTO page_stat_data (id_book, page, start_time, duration, total_pages)
                 VALUES (?, ?, ?, ?, 300)",
            )
            .bind(s.book as i64 + 1)
            .bind(n as i64)
            .bind(BASE + i64::from(s.day) * DAY + 3_600 + n as i64)
            .bind(i64::from(s.seconds))
            .execute(&mut conn)
            .await
            .unwrap();
        }
        conn.close().await.unwrap();
        path
    }

    async fn library(storage: &Storage) {
        for book in 0..3 {
            let id = storage
                .upsert_book(
                    &Book {
                        title: Some(format!("book {book}")),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .unwrap();
            storage
                .link_device_book(&md5_of(book), id, LinkedBy::Auto)
                .await
                .unwrap();
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    proptest! {
        #[test]
        fn minutes_are_absent_exactly_when_the_device_measured_nothing(
            sessions in proptest::collection::vec(session(), 0..12),
            (a, b) in (0u8..20, 0u8..20),
        ) {
            let (from, to) = (a.min(b), a.max(b));
            rt().block_on(async {
                let tmp = tempfile::tempdir().unwrap();
                let storage = Storage::connect("sqlite::memory:").await.unwrap();
                library(&storage).await;
                let db = device(tmp.path(), &sessions).await;

                import_statistics(&storage, &db).await.unwrap();

                let range = DayRange::new(
                    &day_of_unix(BASE + i64::from(from) * DAY).unwrap(),
                    &day_of_unix(BASE + i64::from(to) * DAY).unwrap(),
                ).unwrap();

                let mut per_day: BTreeMap<(usize, u8), i64> = BTreeMap::new();
                for s in &sessions {
                    if s.day >= from && s.day <= to {
                        *per_day.entry((s.book, s.day)).or_default() += i64::from(s.seconds);
                    }
                }
                let want: Option<i64> = if per_day.is_empty() {
                    None
                } else {
                    Some(per_day.values().map(|&sec| minutes_of(sec)).sum())
                };

                let sum = storage.activity_summary(&range).await.unwrap();
                prop_assert_eq!(sum.minutes, want);

                if per_day.is_empty() {
                    prop_assert_eq!(
                        sum.minutes, None,
                        "a period the device never measured must not report zero"
                    );
                } else {
                    prop_assert!(
                        sum.minutes.is_some(),
                        "a period the device did measure must report a number, \
                         even when every session was too short to round up"
                    );
                }

                prop_assert_eq!(sum.activity_days, {
                    let mut days: Vec<u8> = per_day.keys().map(|&(_, d)| d).collect();
                    days.sort_unstable();
                    days.dedup();
                    days.len() as i64
                });
                Ok(())
            })?;
        }
    }
}
