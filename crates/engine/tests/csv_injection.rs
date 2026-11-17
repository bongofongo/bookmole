//! A library whose free text all begins with a formula trigger, exported
//! through both CSV files a spreadsheet is likely to open. No cell may start
//! a formula except the Goodreads ISBN, whose `="…"` armour is deliberate.

mod common;

use bookmole::goodreads::ImportOptions;
use bookmole::{
    HistoryCompare, HistoryGroup, HistoryMetric, HistorySpan, HistorySplit, SeriesQuery,
};

const TRIGGERS: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// One book per trigger; title, author, shelf and review each start with it.
const HOSTILE: [(&str, &str, &str, &str); 4] = [
    (
        "=HYPERLINK(\"https://example.invalid\",\"Middlemarch\")",
        "=1+1 Eliot",
        "=cmd|' /C calc'!A0",
        "=WEBSERVICE(\"https://example.invalid\")",
    ),
    ("+SUM(1,2) Persuasion", "+Austen", "+shelf", "+review"),
    ("-2+3 Cranford", "-Gaskell", "-shelf", "-review"),
    ("@SUM(1) Villette", "@Bronte", "@shelf", "@review"),
];

fn goodreads_csv() -> String {
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record([
        "Book Id",
        "Title",
        "Author",
        "My Rating",
        "Date Read",
        "Date Added",
        "Bookshelves",
        "Exclusive Shelf",
        "My Review",
        "Read Count",
    ])
    .unwrap();
    for (i, (title, author, shelf, review)) in HOSTILE.iter().enumerate() {
        let id = (9_100_001 + i).to_string();
        w.write_record([
            id.as_str(),
            title,
            author,
            "0",
            "2026/03/01",
            "2026/01/01",
            shelf,
            "read",
            review,
            "1",
        ])
        .unwrap();
    }
    String::from_utf8(w.into_inner().unwrap()).unwrap()
}

fn cells(text: &str) -> Vec<(Vec<String>, Vec<String>)> {
    let mut r = csv::Reader::from_reader(text.as_bytes());
    let header: Vec<String> = r.headers().unwrap().iter().map(String::from).collect();
    r.records()
        .map(|rec| {
            let rec = rec.unwrap();
            (header.clone(), rec.iter().map(String::from).collect())
        })
        .collect()
}

fn has_cell(text: &str, value: &str) -> bool {
    cells(text)
        .iter()
        .any(|(_, row)| row.iter().any(|c| c == value))
}

fn assert_inert(name: &str, text: &str, armoured: &[&str]) {
    for (header, row) in cells(text) {
        for (column, cell) in header.iter().zip(&row) {
            if armoured.contains(&column.as_str()) && !cell.is_empty() {
                let digits = cell
                    .strip_prefix("=\"")
                    .and_then(|c| c.strip_suffix('"'))
                    .unwrap_or_else(|| panic!("{name}: {column} is not armoured: {cell:?}"));
                assert!(
                    digits.chars().all(|c| c.is_ascii_alphanumeric()),
                    "{name}: {column} carries more than an ISBN: {cell:?}"
                );
                continue;
            }
            assert!(
                !cell.starts_with(TRIGGERS),
                "{name}: {column} starts a formula: {cell:?}"
            );
        }
    }
}

#[tokio::test]
async fn every_free_text_cell_of_every_export_is_inert() {
    let (tmp, engine) = common::engine().await;
    let source = tmp.path().join("hostile.csv");
    std::fs::write(&source, goodreads_csv()).unwrap();
    let report = engine
        .import_goodreads(&source, ImportOptions::default())
        .await
        .unwrap();
    assert_eq!(report.created(), HOSTILE.len(), "{report:?}");

    let (goodreads, _) = engine.export_goodreads().await.unwrap();
    for (title, author, shelf, review) in HOSTILE {
        for text in [title, author, shelf, review] {
            assert!(
                has_cell(&goodreads, &format!("'{text}")),
                "{text:?} did not reach the Goodreads export guarded:\n{goodreads}"
            );
        }
    }
    assert_inert("goodreads", &goodreads, &["ISBN"]);

    let year = |y: &str| bookmole::history::midnight(bookmole::history::parse_day(y).unwrap());
    let saved = engine
        .export_history_series(&SeriesQuery {
            metric: HistoryMetric::BooksFinished,
            group: HistoryGroup::Month,
            split: HistorySplit::Book,
            compare: HistoryCompare::Nothing,
            span: HistorySpan::new(Some(year("2026-01-01")), Some(year("2027-01-01"))).unwrap(),
        })
        .await
        .unwrap();
    let history = std::fs::read_to_string(&saved.path).unwrap();
    for (title, ..) in HOSTILE {
        assert!(
            has_cell(&history, &format!("'{title}")),
            "{title:?} did not reach the history export guarded:\n{history}"
        );
    }
    assert_inert("history", &history, &[]);
}
