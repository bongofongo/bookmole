//! A folder of books handed to the import, the way a frontend walks it:
//! expand the folder, then import each file it found as new (`ImportFile {
//! new: true }`), in the order the walk returned them.
//!
//! The folder is the 49 Gutenberg epubs under `corpus/epub`, which hold four
//! pairs sharing a title. Walking this path once gave two books too few,
//! because two of the four pairs are two **volumes** of a set and the
//! title match filed each second volume as a copy of the first. The other two
//! pairs are two editions of one book and must stay one book. So: 47 books,
//! the two sets four books, the two editions two.
//!
//! **The whole folder is expanded and only the eight paired files imported.**
//! Importing the whole folder was run once (119 s in a debug build, nearly
//! all of it cover decoding in the big books) and is too slow to pay on every
//! run for 41 files whose titles share nothing; the eight are where the answer
//! can change.

mod common;

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use bookmole::FileImportOptions;
use bookmole::providers::ProviderId;

fn skip(reason: &str) {
    if std::env::var("BOOKMOLE_REQUIRE_FIXTURES").is_ok() {
        panic!("BOOKMOLE_REQUIRE_FIXTURES set but {reason}");
    }
    eprintln!("SKIPPED: {reason}");
}

#[tokio::test]
async fn the_corpus_folder_imports_as_its_books_with_each_volume_its_own() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/epub");
    if !dir.is_dir() {
        skip("corpus/epub is absent");
        return;
    }
    let (_tmp, engine) = common::engine().await;
    // No network in tests, ever: a Gutenberg epub carries no ISBN, but an
    // import that found one would otherwise ask Open Library.
    engine
        .set_search_provider_enabled(ProviderId::OpenLibrary, false)
        .await
        .expect("provider off");

    let expanded = engine
        .expand_book_paths(vec![dir.clone()])
        .await
        .expect("the folder expands");
    assert_eq!(expanded.files.len(), 49, "{:?}", expanded.skipped);
    assert_eq!(expanded.cut_off_at, None);
    assert!(expanded.calibre_libraries.is_empty());

    const PAIRED: [&str; 8] = [
        "1342.epub",
        "2147.epub",
        "2148.epub",
        "21000.epub",
        "2229.epub",
        "39764.epub",
        "40061.epub",
        "42671.epub",
    ];
    let mut book_of: HashMap<String, i64> = HashMap::new();
    for file in expanded.files.iter().filter(|f| {
        f.file_name()
            .is_some_and(|n| PAIRED.iter().any(|p| n.to_string_lossy() == *p))
    }) {
        let report = engine
            .import_file(file, FileImportOptions { new: true })
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        book_of.insert(name, report.book_id.expect("imported as new, so a book"));
    }
    let books: BTreeSet<i64> = book_of.values().copied().collect();
    let id = |name: &str| book_of[name];

    // Two volumes of Poe, two of the Anabasis: four books.
    assert_ne!(id("2147.epub"), id("2148.epub"), "Poe, Volume 1 and 2");
    assert_ne!(
        id("39764.epub"),
        id("40061.epub"),
        "Κύρου Ανάβασις, Τόμος 1 and 2"
    );
    // Two editions of Pride and Prejudice, two of Faust I: two books.
    assert_eq!(id("1342.epub"), id("42671.epub"), "Pride and Prejudice");
    assert_eq!(id("21000.epub"), id("2229.epub"), "Faust, erster Teil");
    assert_eq!(book_of.len(), PAIRED.len());
    assert_eq!(books.len(), 6);
}
