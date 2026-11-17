//! The KOReader `statistics.sqlite3` fixture, emitted as SQL text rather than a binary database.
//! Expected totals accumulate as rows are written, never read back, to stay an independent oracle.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};

pub const GENERATOR_VERSION: u32 = 1;

const SCHEMA_VERSION: i64 = 20221111;

const BASE_DAY: i64 = 20458;

/// The plugin's own DDL, quoted exactly; only whitespace is reformatted.
const SCHEMA: &str = "\
PRAGMA user_version=20221111;

CREATE TABLE IF NOT EXISTS book
    (
        id integer PRIMARY KEY autoincrement,
        title text,
        authors text,
        notes      integer,
        last_open  integer,
        highlights integer,
        pages      integer,
        series text,
        language text,
        md5 text,
        total_read_time  integer,
        total_read_pages integer
    );

CREATE TABLE IF NOT EXISTS page_stat_data
    (
        id_book     integer,
        page        integer NOT NULL DEFAULT 0,
        start_time  integer NOT NULL DEFAULT 0,
        duration    integer NOT NULL DEFAULT 0,
        total_pages integer NOT NULL DEFAULT 0,
        UNIQUE (id_book, page, start_time),
        FOREIGN KEY(id_book) REFERENCES book(id)
    );

CREATE TABLE IF NOT EXISTS numbers
    (
        number INTEGER PRIMARY KEY
    );

CREATE UNIQUE INDEX IF NOT EXISTS book_title_authors_md5 ON book(title, authors, md5);
CREATE INDEX IF NOT EXISTS page_stat_data_start_time ON page_stat_data(start_time);
";

pub struct Options {
    pub seed: u64,
}

struct BookSpec {
    title: &'static str,
    /// `None` models a statistics row the plugin wrote with no md5: nothing to join on.
    md5: Option<&'static str>,
    in_library: bool,
    days: &'static [(i64, &'static [(i64, i64)])],
}

/// Written out, not randomised: every row is a case. The seed drives only the filler rows.
const BOOKS: &[BookSpec] = &[
    BookSpec {
        title: "Middlemarch",
        md5: Some("3f9c1e7a5b2d4068a1c3e5f7092b4d6e"),
        in_library: true,
        days: &[
            (0, &[(10, 300), (11, 240), (12, 180)]),
            (1, &[(13, 600), (13, 120), (14, 90)]),
            (4, &[(15, 3600)]),
        ],
    },
    // A real session of nineteen seconds: records `Some(0)` minutes, never `None`.
    BookSpec {
        title: "Brief Encounter",
        md5: Some("7d2a9e4c1b6f3058c2e4a6b8d0f1a3c5"),
        in_library: true,
        days: &[(2, &[(1, 19)])],
    },
    BookSpec {
        title: "A Book We Never Imported",
        md5: Some("b4e8a2c6d0f3157e9a1c3e5b7d9f0a2c"),
        in_library: false,
        days: &[(0, &[(3, 900)])],
    },
    BookSpec {
        title: "No Checksum Here",
        md5: None,
        in_library: false,
        days: &[(1, &[(2, 450)])],
    },
];

#[derive(serde::Serialize)]
struct Expected {
    generator_version: u32,
    schema_version: i64,
    books: Vec<ExpectedBook>,
}

#[derive(serde::Serialize)]
struct ExpectedBook {
    title: String,
    md5: Option<String>,
    in_library: bool,
    days: Vec<ExpectedDay>,
}

#[derive(serde::Serialize)]
struct ExpectedDay {
    day: String,
    seconds: i64,
    pages: i64,
    /// Seconds rounded to the nearest minute: what `reading_events.minutes` must hold.
    minutes: i64,
}

pub fn generate(out: &Path, opts: &Options) -> std::io::Result<usize> {
    std::fs::create_dir_all(out)?;
    let mut rng = ChaCha8Rng::seed_from_u64(opts.seed);

    let mut sql = String::from(SCHEMA);
    sql.push('\n');
    let mut expected = Expected {
        generator_version: GENERATOR_VERSION,
        schema_version: SCHEMA_VERSION,
        books: Vec::new(),
    };

    for (i, spec) in BOOKS.iter().enumerate() {
        let id = i as i64 + 1;
        let md5_sql = match spec.md5 {
            Some(m) => format!("'{m}'"),
            None => "NULL".to_string(),
        };
        sql.push_str(&format!(
            "INSERT INTO book (id, title, authors, md5, pages, language) \
             VALUES ({id}, '{}', 'A. Writer', {md5_sql}, 300, 'en');\n",
            spec.title.replace('\'', "''")
        ));

        let mut per_day: BTreeMap<i64, (i64, std::collections::BTreeSet<i64>)> = BTreeMap::new();

        for (day_off, pages) in spec.days {
            let day_start = (BASE_DAY + day_off) * 86_400;
            for (n, (page, seconds)) in pages.iter().enumerate() {
                // Distinct start times per page, so repeat rows satisfy the UNIQUE constraint as on a device.
                let start = day_start + 3_600 + (n as i64 * 907) + (rng.next_u32() % 60) as i64;
                sql.push_str(&format!(
                    "INSERT INTO page_stat_data (id_book, page, start_time, duration, total_pages) \
                     VALUES ({id}, {page}, {start}, {seconds}, 300);\n"
                ));
                let e = per_day.entry(*day_off).or_default();
                e.0 += seconds;
                e.1.insert(*page);
            }
        }

        // Dead-clock row: `start_time` 0 is the column default. Emitted for the first book only and
        // deliberately absent from `expected`.
        if i == 0 {
            sql.push_str(&format!(
                "INSERT INTO page_stat_data (id_book, page, start_time, duration, total_pages) \
                 VALUES ({id}, 99, 0, 500, 300);\n"
            ));
        }

        expected.books.push(ExpectedBook {
            title: spec.title.to_string(),
            md5: spec.md5.map(str::to_string),
            in_library: spec.in_library,
            days: per_day
                .into_iter()
                .map(|(off, (seconds, pages))| ExpectedDay {
                    day: ymd(BASE_DAY + off),
                    seconds,
                    pages: pages.len() as i64,
                    minutes: (seconds + 30) / 60,
                })
                .collect(),
        });
    }

    let mut f = std::fs::File::create(out.join("statistics.sql"))?;
    f.write_all(sql.as_bytes())?;

    let mut e = std::fs::File::create(out.join("expected.json"))?;
    e.write_all(serde_json::to_string_pretty(&expected)?.as_bytes())?;
    e.write_all(b"\n")?;

    Ok(BOOKS.len())
}

pub(crate) fn ymd(days: i64) -> String {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

pub struct CorpusStatsBook {
    pub title: String,
    pub authors: String,
    pub language: String,
    pub md5: String,
    pub pages: i64,
    pub highlights: i64,
    pub notes: i64,
    pub rows: Vec<(i64, i64, i64)>,
}

pub fn write_corpus_stats(out: &Path, books: &[CorpusStatsBook]) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out)?;
    let mut sql = String::from(SCHEMA);
    sql.push('\n');
    let mut expected = Expected {
        generator_version: GENERATOR_VERSION,
        schema_version: SCHEMA_VERSION,
        books: Vec::new(),
    };
    let q = |s: &str| s.replace('\'', "''").replace('\0', "");
    for (i, b) in books.iter().enumerate() {
        let id = i as i64 + 1;
        let total_time: i64 = b.rows.iter().map(|r| r.2).sum();
        let distinct: std::collections::BTreeSet<i64> = b.rows.iter().map(|r| r.0).collect();
        let last_open = b.rows.iter().map(|r| r.1 + r.2).max().unwrap_or(0);
        sql.push_str(&format!(
            "INSERT INTO book (id, title, authors, notes, last_open, highlights, pages, series, language, md5, total_read_time, total_read_pages) \
             VALUES ({id}, '{}', '{}', {}, {last_open}, {}, {}, NULL, '{}', '{}', {total_time}, {});\n",
            q(&b.title),
            q(&b.authors),
            b.notes,
            b.highlights,
            b.pages,
            q(&b.language),
            b.md5,
            distinct.len(),
        ));
        let mut per_day: BTreeMap<i64, (i64, std::collections::BTreeSet<i64>)> = BTreeMap::new();
        for (page, start, dur) in &b.rows {
            sql.push_str(&format!(
                "INSERT INTO page_stat_data (id_book, page, start_time, duration, total_pages) \
                 VALUES ({id}, {page}, {start}, {dur}, {});\n",
                b.pages
            ));
            let e = per_day.entry(start.div_euclid(86_400)).or_default();
            e.0 += dur;
            e.1.insert(*page);
        }
        expected.books.push(ExpectedBook {
            title: b.title.clone(),
            md5: Some(b.md5.clone()),
            in_library: true,
            days: per_day
                .into_iter()
                .map(|(day, (seconds, pages))| ExpectedDay {
                    day: ymd(day),
                    seconds,
                    pages: pages.len() as i64,
                    minutes: (seconds + 30) / 60,
                })
                .collect(),
        });
    }
    let sql_path = out.join("statistics.sql");
    std::fs::write(&sql_path, sql.as_bytes())?;
    let exp_path = out.join("expected.json");
    std::fs::write(
        &exp_path,
        format!("{}\n", serde_json::to_string_pretty(&expected)?),
    )?;
    Ok(vec![sql_path, exp_path])
}

pub fn default_out() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("engine/tests/fixtures/koreader/statistics")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_agrees_with_known_dates() {
        assert_eq!(ymd(0), "1970-01-01");
        assert_eq!(ymd(BASE_DAY), "2026-01-05");
        assert_eq!(ymd(BASE_DAY + 1), "2026-01-06");
        assert_eq!(ymd(19_782), "2024-02-29");
    }
}
