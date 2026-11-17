//! The embedded offline index of well-known works: **the one test that
//! opens the real file.** Every other test of suggestions and correction runs
//! against a fixture built in code.
//!
//! It checks the bytes are the ones `wellknown.json` records, and measures the
//! three budgets it was built to — built in 5 s, a lookup in 15 ms, 100 MB on
//! disk — printing them (`--nocapture`). The time
//! budgets are the shipped build's: an unoptimised test build is given a
//! multiple of them, and still fails if it is wildly over.

#![cfg(feature = "wellknown")]

use std::time::{Duration, Instant};

use bookmole::search::wellknown::{
    WellKnown, embedded_bytes, embedded_record, gunzip, parse_works,
};
use sha2::{Digest, Sha256};

/// The budgets.
const BUILD: Duration = Duration::from_secs(5);
const LOOKUP: Duration = Duration::from_millis(15);
const DISK: u64 = 100 * 1024 * 1024;
const EMBEDDED: usize = 12 * 1024 * 1024;

/// How far over a time budget a run may land before it fails. The budgets are
/// the shipped build's, measured and reported by this test in release; the
/// assertion is a tripwire for a gross regression, not the measurement — a
/// test machine running the whole suite at once is not the user's, and an
/// unoptimised build is several times slower than the shipped one.
fn slack() -> u32 {
    if cfg!(debug_assertions) { 20 } else { 2 }
}

#[tokio::test]
async fn the_embedded_index_is_the_recorded_file_and_opens_within_budget() {
    let bytes = embedded_bytes().expect("the wellknown feature embeds a file");
    let record = embedded_record().expect("wellknown.json parses");
    let sha: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        sha, record.sha256,
        "the embedded file is not the one recorded"
    );
    assert_eq!(bytes.len(), record.bytes);
    assert!(
        bytes.len() <= EMBEDDED,
        "{} bytes embedded, over the 12 MB budget",
        bytes.len()
    );
    let works = parse_works(&gunzip(bytes).unwrap()).unwrap();
    assert_eq!(works.len(), record.works, "the file holds what it says");
    // Sorted by Q-number, one line a work: the file is diffable by tool.
    assert!(works.windows(2).all(|w| w[0].wikidata < w[1].wikidata));

    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let wk = WellKnown::on_disk(dir.path(), &record, bytes)
        .await
        .unwrap();
    let build = started.elapsed();
    assert_eq!(wk.count().await.unwrap() as usize, record.works);
    let disk: u64 = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
        .sum();

    // Reopened, not rebuilt.
    let started = Instant::now();
    let wk = WellKnown::on_disk(dir.path(), &record, bytes)
        .await
        .unwrap();
    let reopen = started.elapsed();

    // The slowest of a spread of lookups, each kind the search makes.
    let words = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    let mut slowest: Vec<(&str, Duration)> = Vec::new();
    let mut time =
        |kind: &'static str, d: Duration| match slowest.iter_mut().find(|(k, _)| *k == kind) {
            Some(e) => e.1 = e.1.max(d),
            None => slowest.push((kind, d)),
        };
    for q in [
        "dnue herbrt",
        "the brothrs karamazov",
        "frankenstien",
        "jane eyer",
        "the",
    ] {
        let t = Instant::now();
        wk.candidates(&words(q), 40).await.unwrap();
        time("candidates", t.elapsed());
    }
    for p in ["d", "the", "harry pot", "tolkien", "der wüsten"] {
        let t = Instant::now();
        wk.suggest(p, 8).await.unwrap();
        time("suggest", t.elapsed());
    }
    for _ in 0..3 {
        let t = Instant::now();
        wk.notability(&[
            ("/works/OL893415W".into(), "Dune".into(), None),
            (
                "/works/OL1W".into(),
                "The Hobbit".into(),
                Some("J. R. R. Tolkien".into()),
            ),
        ])
        .await
        .unwrap();
        time("notability", t.elapsed());
        let t = Instant::now();
        wk.authors_named(&words("frank herbert")).await.unwrap();
        time("names", t.elapsed());
    }
    let worst = slowest.iter().map(|(_, d)| *d).max().unwrap_or_default();
    println!(
        "offline index: {} works, {} bytes embedded, {} bytes on disk, built in {:.2?}, reopened in {:.2?}, slowest lookups {:?} ({} build)",
        record.works,
        bytes.len(),
        disk,
        build,
        reopen,
        slowest,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    assert!(disk <= DISK, "{disk} bytes on disk, over the 100 MB budget");
    assert!(build <= BUILD * slack(), "built in {build:?}");
    assert!(worst <= LOOKUP * slack(), "a lookup took {worst:?}");

    // What the benchmark's typos need is in it.
    let dune = wk.suggest("dune", 3).await.unwrap();
    assert!(
        dune.iter()
            .any(|h| h.author.as_deref() == Some("Frank Herbert"))
    );
}
