//! Imports a walked folder of epubs as new books; only the title-sharing pairs are imported,
//! because importing all of them is too slow.

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

    assert_ne!(id("2147.epub"), id("2148.epub"), "Poe, Volume 1 and 2");
    assert_ne!(
        id("39764.epub"),
        id("40061.epub"),
        "Κύρου Ανάβασις, Τόμος 1 and 2"
    );
    assert_eq!(id("1342.epub"), id("42671.epub"), "Pride and Prejudice");
    assert_eq!(id("21000.epub"), id("2229.epub"), "Faust, erster Teil");
    assert_eq!(book_of.len(), PAIRED.len());
    assert_eq!(books.len(), 6);
}
