//! Owned files and the three dedup levels, asserted separately by name.
//! Books come from ISBN-less epubs or filenames because import would otherwise look up ISBNs
//! over the network.

use std::path::{Path, PathBuf};

use bookmole::{
    Book, Engine, FileImportOptions, FileMatch, FileOutcome, KeptCopy, LearnedFrom, PlaceKind,
    Tenure,
};

mod common;
use common::{engine, write_isbnless_epub, write_isbnless_epub_by};

fn isbn_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/koreader/synthetic/Gen-Isbn-Match.epub")
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

fn stored_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries {
        let p = entry.unwrap().path();
        if p.is_dir() {
            out.extend(stored_files(&p));
        } else {
            out.push(p);
        }
    }
    out.sort();
    out
}

async fn keep(engine: &Engine, path: &Path) -> KeptCopy {
    let at = std::path::absolute(path).unwrap().display().to_string();
    let copy = engine
        .copy_for_place(PlaceKind::Path, &at)
        .await
        .unwrap()
        .expect("the file was imported, so its path is a place");
    engine.keep_copy_here(copy.id).await.unwrap()
}

fn files_dir(tmp: &tempfile::TempDir) -> PathBuf {
    tmp.path().join("database/files")
}

async fn library_size(engine: &Engine) -> usize {
    engine
        .storage()
        .list_books(&bookmole::BookQuery::new(1000, bookmole::BookSort::Title))
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn an_imported_file_is_pointed_to_and_nothing_is_copied() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("Some Downloaded Name (1).epub");
    write_isbnless_epub(&src, "A Book With No ISBN");

    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    assert_eq!(report.outcome, FileOutcome::Stored);
    assert!(report.created_book);
    let book_id = report.book_id.unwrap();

    assert!(engine.book_files(book_id).await.unwrap().is_empty());
    assert!(stored_files(&files_dir(&tmp)).is_empty());

    let copies = engine.copies_for_book(book_id).await.unwrap();
    assert_eq!(copies.len(), 1);
    let copy = &copies[0];
    assert_eq!(copy.form, Some(bookmole::CopyForm::File));
    assert_eq!(copy.tenure, Tenure::Own);
    assert_eq!(copy.learned_from, LearnedFrom::FileImport);
    assert_eq!(
        copy.have_now, None,
        "only the user says whether they have it"
    );
    assert_eq!(copy.places.len(), 1);
    let place = &copy.places[0];
    assert_eq!(place.kind, PlaceKind::Path);
    let at = std::path::absolute(&src).unwrap();
    assert_eq!(place.reference, at.display().to_string());
    assert_eq!(place.last_hash.as_deref(), Some(report.sha256.as_str()));
    assert!(place.last_mtime.is_some());
    assert_eq!(report.stored_path.as_ref(), Some(&at));
    assert!(engine.book_standing(book_id).await.unwrap().in_library);
}

#[tokio::test]
async fn the_same_path_with_changed_bytes_is_the_same_copy() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("Annotated.pdf");
    write(&src, &bookmole::pdf::synthetic_pdf(100, None));
    let first = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    let book_id = first.book_id.unwrap();

    write(&src, &bookmole::pdf::synthetic_pdf(101, None));
    let second = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    assert_ne!(first.sha256, second.sha256);
    assert_eq!(second.book_id, Some(book_id));
    assert!(!second.created_book);
    assert_eq!(second.outcome, FileOutcome::Stored);
    assert_eq!(library_size(&engine).await, 1);

    let copies = engine.copies_for_book(book_id).await.unwrap();
    assert_eq!(copies.len(), 1, "a changed hash must not sprout a copy");
    assert_eq!(copies[0].places.len(), 1);
    assert_eq!(
        copies[0].places[0].last_hash.as_deref(),
        Some(second.sha256.as_str())
    );

    let third = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    assert_eq!(third.outcome, FileOutcome::AlreadyOwned);
    assert_eq!(engine.copies_for_book(book_id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn keep_a_copy_here_stores_the_file_and_adds_a_store_place() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("Some Downloaded Name (1).epub");
    write_isbnless_epub(&src, "A Book With No ISBN");
    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    let book_id = report.book_id.unwrap();

    let kept = keep(&engine, &src).await;
    assert_eq!(kept.sha256, report.sha256);
    assert!(!kept.already_present);
    assert_eq!(kept.attached, bookmole::PlaceAttach::Added);

    let files = engine.book_files(book_id).await.unwrap();
    assert_eq!(files.len(), 1);
    let file = &files[0];
    assert_eq!(file.format, "epub");
    assert_eq!(
        file.original_name.as_deref(),
        Some("Some Downloaded Name (1).epub")
    );
    let stored = engine.file_path(file);
    assert_eq!(kept.stored_path, stored);
    assert!(stored.starts_with(files_dir(&tmp).join(&file.sha256[..2])));
    assert_eq!(
        std::fs::read(&stored).unwrap(),
        std::fs::read(&src).unwrap(),
        "the copy is the file, byte for byte"
    );
    assert!(src.is_file(), "keeping is copying, not moving");

    let copies = engine.copies_for_book(book_id).await.unwrap();
    assert_eq!(copies.len(), 1, "the same copy, now in two places");
    let mut kinds: Vec<_> = copies[0].places.iter().map(|p| p.kind).collect();
    kinds.sort_by_key(|k| k.as_str());
    assert_eq!(kinds, [PlaceKind::Path, PlaceKind::Store]);

    let again = keep(&engine, &src).await;
    assert!(again.already_present);
    assert_eq!(again.attached, bookmole::PlaceAttach::Present);
    assert_eq!(stored_files(&files_dir(&tmp)).len(), 1);
}

#[tokio::test]
async fn a_copy_with_no_file_here_cannot_be_kept() {
    let (_tmp, engine) = engine().await;
    let book = engine
        .save_book(&Book {
            title: Some("Middlemarch".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let copy = engine
        .ensure_unrecorded_copy(book.id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(engine.keep_copy_here(copy.id).await.is_err());
}

#[tokio::test]
async fn an_imported_file_records_the_identity_the_device_will_use() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("offline.epub");
    write_isbnless_epub(&src, "A Book With No ISBN");

    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    let md5 = bookmole::partial_md5(&src).unwrap();
    let linked = engine
        .storage()
        .find_book_by_partial_md5(&md5)
        .await
        .unwrap()
        .expect("the file's device identity must be recorded");
    assert_eq!(linked.id, report.book_id);
}

#[tokio::test]
async fn level_1_the_same_bytes_are_owned_once() {
    let (tmp, engine) = engine().await;
    let first = tmp.path().join("A Book With No ISBN.epub");
    write_isbnless_epub(&first, "A Book With No ISBN");
    let second = tmp.path().join("a-book-with-no-isbn (copy).epub");
    std::fs::copy(&first, &second).unwrap();

    let a = engine
        .import_file(&first, FileImportOptions::default())
        .await
        .unwrap();
    let b = engine
        .import_file(&second, FileImportOptions::default())
        .await
        .unwrap();

    assert_eq!(b.outcome, FileOutcome::AlreadyOwned);
    assert_eq!(b.matched_by, Some(FileMatch::Sha256));
    assert_eq!(b.book_id, a.book_id);
    assert!(!b.created_book);
    assert_eq!(a.sha256, b.sha256);

    assert_eq!(
        library_size(&engine).await,
        1,
        "the second copy must not become a second book"
    );
    let copies = engine.copies_for_book(a.book_id.unwrap()).await.unwrap();
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].places.len(), 2);
    assert!(stored_files(&files_dir(&tmp)).is_empty());
}

#[tokio::test]
async fn level_1_holds_even_when_the_caller_names_another_book() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("A Book With No ISBN.epub");
    write_isbnless_epub(&src, "A Book With No ISBN");
    let first = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    keep(&engine, &src).await;

    let other = engine
        .save_book(&Book {
            title: Some("A Completely Different Book".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let report = engine
        .add_file_to_book(other.id.unwrap(), &src)
        .await
        .unwrap();

    assert_eq!(report.outcome, FileOutcome::AlreadyOwned);
    assert_eq!(report.book_id, first.book_id);
    assert!(
        engine
            .book_files(other.id.unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .copies_for_book(other.id.unwrap())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn level_2_two_formats_are_two_files_of_one_book() {
    let (tmp, engine) = engine().await;
    let epub = tmp.path().join("A Book With No ISBN.epub");
    write_isbnless_epub(&epub, "A Book With No ISBN");
    let first = engine
        .import_file(&epub, FileImportOptions::default())
        .await
        .unwrap();
    let book_id = first.book_id.unwrap();

    let azw3 = tmp.path().join("A Book With No ISBN.azw3");
    write(&azw3, b"a different container of the same book");
    let second = engine.add_file_to_book(book_id, &azw3).await.unwrap();

    assert_eq!(second.outcome, FileOutcome::Stored);
    assert_eq!(second.book_id, Some(book_id));
    assert_ne!(first.sha256, second.sha256);
    assert_eq!(library_size(&engine).await, 1);

    let copies = engine.copies_for_book(book_id).await.unwrap();
    assert_eq!(copies.len(), 2);
    assert!(copies.iter().all(|c| c.places.len() == 1));
    assert!(engine.book_files(book_id).await.unwrap().is_empty());
    assert!(stored_files(&files_dir(&tmp)).is_empty());
}

#[tokio::test]
async fn level_2_is_reached_by_the_title_rung_too() {
    let (tmp, engine) = engine().await;
    let epub = tmp.path().join("A Book With No ISBN.epub");
    write_isbnless_epub(&epub, "A Book With No ISBN");
    let first = engine
        .import_file(&epub, FileImportOptions::default())
        .await
        .unwrap();

    let azw3 = tmp.path().join("A Book With No ISBN.azw3");
    write(&azw3, b"the azw3 of the same book");
    let second = engine
        .import_file(&azw3, FileImportOptions::default())
        .await
        .unwrap();

    assert_eq!(second.matched_by, Some(FileMatch::Title));
    assert_eq!(second.book_id, first.book_id);
    assert!(!second.created_book);
    assert_eq!(library_size(&engine).await, 1);
}

#[tokio::test]
async fn level_3_matches_on_isbn_before_anything_else() {
    let (_tmp, engine) = engine().await;
    let existing = engine
        .save_book(&Book {
            // Nothing like the epub's own title, so only the ISBN can have matched.
            title: Some("Zzz Nothing Like The Title".into()),
            isbn_13: Some("9780316769488".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let report = engine
        .import_file(&isbn_fixture(), FileImportOptions::default())
        .await
        .unwrap();
    assert_eq!(report.matched_by, Some(FileMatch::Isbn));
    assert_eq!(report.book_id, existing.id);
    assert_eq!(library_size(&engine).await, 1);
}

#[tokio::test]
async fn level_3_matches_on_the_device_identity_next() {
    let (tmp, engine) = engine().await;
    let file = tmp.path().join("B00KF1L3.azw3");
    write(&file, b"an ebook the device already knows");

    let existing = engine
        .save_book(&Book {
            title: Some("Zzz Nothing Like The Filename".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let md5 = bookmole::partial_md5(&file).unwrap();
    engine
        .storage()
        .link_device_book(
            &md5,
            existing.id.unwrap(),
            bookmole::storage::LinkedBy::Manual,
        )
        .await
        .unwrap();

    let report = engine
        .import_file(&file, FileImportOptions::default())
        .await
        .unwrap();
    assert_eq!(report.matched_by, Some(FileMatch::Md5));
    assert_eq!(report.book_id, existing.id);
    assert_eq!(library_size(&engine).await, 1);
}

#[tokio::test]
async fn level_3_refuses_to_create_over_a_candidate_and_writes_nothing() {
    let (tmp, engine) = engine().await;
    let near = engine
        .save_book(&Book {
            title: Some("Middlemarch: A Study of Provincial Life".into()),
            // Stored as the origin spelled it; the row the user reads must not be.
            authors: vec!["Eliot, George".into()],
            publish_year: Some(2017),
            ..Default::default()
        })
        .await
        .unwrap();
    let src = tmp.path().join("Middlemarch.epub");
    write_isbnless_epub_by(&src, "Middlemarch", "George Eliot");

    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    assert_eq!(report.outcome, FileOutcome::Unmatched);
    assert_eq!(report.book_id, None);
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.candidates[0].book_id, near.id.unwrap());
    assert_eq!(report.candidates[0].authors_display, ["George Eliot"]);
    assert_eq!(report.candidates[0].publish_year, Some(2017));
    assert!(
        (0.60..0.85).contains(&report.candidates[0].score),
        "the pair has drifted out of the candidate band: {}",
        report.candidates[0].score
    );

    assert_eq!(library_size(&engine).await, 1, "nothing was created");
    assert!(
        stored_files(&files_dir(&tmp)).is_empty(),
        "a refusal must not leave the bytes behind either"
    );
    assert!(report.stored_path.is_none());
}

#[tokio::test]
async fn level_3_creates_when_the_caller_overrides_the_candidates() {
    let (tmp, engine) = engine().await;
    engine
        .save_book(&Book {
            title: Some("Middlemarch: A Study of Provincial Life".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let src = tmp.path().join("Middlemarch.epub");
    write_isbnless_epub(&src, "Middlemarch");

    let report = engine
        .import_file(&src, FileImportOptions { new: true })
        .await
        .unwrap();
    assert_eq!(report.outcome, FileOutcome::Stored);
    assert!(report.created_book);
    assert_eq!(library_size(&engine).await, 2);
    assert!(stored_files(&files_dir(&tmp)).is_empty());
    assert_eq!(
        engine
            .copies_for_book(report.book_id.unwrap())
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn an_unrecognised_file_becomes_a_book_when_there_is_nothing_to_confuse_it_with() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("whatever-the-download-called-it.epub");
    write_isbnless_epub(&src, "A Book With No ISBN");

    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    assert!(report.created_book);
    let book = engine
        .storage()
        .get_book(report.book_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(book.title.as_deref(), Some("A Book With No ISBN"));
}

#[tokio::test]
async fn a_format_we_cannot_read_is_still_ours_to_keep() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("Long Walk to Freedom.azw3");
    write(&src, b"mobi-ish bytes we have no reader for");

    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    assert_eq!(report.outcome, FileOutcome::Stored);
    let book = engine
        .storage()
        .get_book(report.book_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(book.title.as_deref(), Some("Long Walk to Freedom"));
    keep(&engine, &src).await;
    assert_eq!(
        engine.book_files(book.id.unwrap()).await.unwrap()[0].format,
        "azw3"
    );
}

#[tokio::test]
async fn identify_answers_without_writing_anything() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("A Book With No ISBN.epub");
    write_isbnless_epub(&src, "A Book With No ISBN");

    let id = engine.identify_file(&src).await.unwrap();
    assert!(id.matched.is_none());
    assert_eq!(id.format, "epub");
    assert_eq!(id.title.as_deref(), Some("A Book With No ISBN"));
    assert_eq!(id.sha256.len(), 64);
    assert_eq!(id.partial_md5.len(), 32);

    assert_eq!(library_size(&engine).await, 0);
    assert!(stored_files(&files_dir(&tmp)).is_empty());
    assert_eq!(engine.identify_file(&src).await.unwrap().sha256, id.sha256);
}

#[tokio::test]
async fn identifying_something_that_is_not_a_file_is_an_error_not_a_panic() {
    let (tmp, engine) = engine().await;
    let err = engine.identify_file(tmp.path()).await.unwrap_err();
    assert!(
        matches!(err, bookmole::EngineError::InvalidInput(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn merging_two_books_carries_their_files_across() {
    let (tmp, engine) = engine().await;
    let a = tmp.path().join("Middlemarch.epub");
    write_isbnless_epub(&a, "Middlemarch");
    let b = tmp.path().join("Zzz An Unhelpfully Named Scan.azw3");
    write(&b, b"the other copy of the same book");

    let first = engine
        .import_file(&a, FileImportOptions::default())
        .await
        .unwrap();
    let second = engine
        .import_file(&b, FileImportOptions::default())
        .await
        .unwrap();
    let (src, dst) = (second.book_id.unwrap(), first.book_id.unwrap());
    assert_ne!(src, dst, "the fixture must produce two books to merge");
    keep(&engine, &a).await;
    keep(&engine, &b).await;

    let report = engine.merge_books(src, dst).await.unwrap();
    assert_eq!(report.files_moved, 1);

    let files = engine.book_files(dst).await.unwrap();
    assert_eq!(files.len(), 2, "both files are now the survivor's");
    for file in &files {
        assert!(
            engine.file_path(file).is_file(),
            "and the bytes are still there"
        );
    }
    assert_eq!(stored_files(&files_dir(&tmp)).len(), 2);
}

#[tokio::test]
async fn deleting_a_book_removes_the_files_it_owned() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("A Book With No ISBN.epub");
    write_isbnless_epub(&src, "A Book With No ISBN");
    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    let stored = keep(&engine, &src).await.stored_path;
    assert!(stored.is_file());

    engine.delete_book(report.book_id.unwrap()).await.unwrap();

    assert!(!stored.exists(), "the bytes go with the book");
    assert!(stored_files(&files_dir(&tmp)).is_empty());
    assert!(
        engine
            .storage()
            .book_file(&report.sha256)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_file_can_be_given_up_and_giving_it_up_twice_is_a_no_op() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("A Book With No ISBN.epub");
    write_isbnless_epub(&src, "A Book With No ISBN");
    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    keep(&engine, &src).await;

    assert!(engine.remove_file(&report.sha256).await.unwrap());
    assert!(!engine.remove_file(&report.sha256).await.unwrap());
    assert!(stored_files(&files_dir(&tmp)).is_empty());
    assert!(
        engine
            .book_files(report.book_id.unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(library_size(&engine).await, 1);
}

#[tokio::test]
async fn attaching_to_a_book_that_does_not_exist_is_an_error() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("A Book With No ISBN.epub");
    write_isbnless_epub(&src, "A Book With No ISBN");
    let err = engine.add_file_to_book(9_999, &src).await.unwrap_err();
    assert!(matches!(err, bookmole::EngineError::NotFound(_)), "{err:?}");
    assert!(stored_files(&files_dir(&tmp)).is_empty());
}

fn toc_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/koreader/synthetic/Gen-Toc-Chapters.epub")
}

#[tokio::test]
async fn a_chapter_list_follows_the_file_the_book_owns() {
    let (tmp, engine) = engine().await;

    let plain = tmp.path().join("A Book With No ISBN.epub");
    write_isbnless_epub(&plain, "A Book With No ISBN");
    let report = engine
        .import_file(&plain, FileImportOptions::default())
        .await
        .unwrap();
    let book_id = report.book_id.unwrap();
    keep(&engine, &plain).await;

    let first = engine.table_of_contents(book_id).await.unwrap().unwrap();
    assert!(first.entries.is_empty());
    assert_eq!(first.sha256, report.sha256);

    engine.remove_file(&report.sha256).await.unwrap();
    std::fs::remove_file(&plain).unwrap();
    assert!(
        engine.table_of_contents(book_id).await.unwrap().is_none(),
        "a book owning no file this can read has no chapter list to give"
    );
    let better = engine
        .add_file_to_book(book_id, &toc_fixture())
        .await
        .unwrap();
    keep(&engine, &toc_fixture()).await;

    let toc = engine.table_of_contents(book_id).await.unwrap().unwrap();
    assert_eq!(
        toc.sha256, better.sha256,
        "the answer names the file it read"
    );
    assert_eq!(
        toc.entries
            .iter()
            .map(|e| e.label.as_str())
            .collect::<Vec<_>>(),
        vec![
            "One: The Beginning",
            "Two: The Middle",
            "Two, part two",
            "Three: The End",
        ]
    );
    assert_eq!(toc.entries[2].depth, 1);
}

#[tokio::test]
async fn a_book_with_no_epub_has_no_chapter_list() {
    let (_tmp, engine) = engine().await;
    let book = engine
        .save_book(&Book {
            title: Some("Nothing Owned".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        engine
            .table_of_contents(book.id.unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn an_attached_pdf_gives_the_book_its_length() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("Long Walk to Freedom.pdf");
    write(&src, &bookmole::pdf::synthetic_pdf(312, None));

    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    assert_eq!(report.outcome, FileOutcome::Stored);
    let id = report.book_id.unwrap();

    let book = engine.storage().get_book(id).await.unwrap().unwrap();
    assert_eq!(book.page_count, Some(312));
    assert_eq!(book.title.as_deref(), Some("Long Walk to Freedom"));
    keep(&engine, &src).await;
    assert_eq!(engine.book_files(id).await.unwrap()[0].format, "pdf");

    let prov = engine.field_provenance(id).await.unwrap();
    let page_count = prov.iter().find(|f| f.field == "page_count").unwrap();
    assert_eq!(page_count.source, "pdf");
}

/// A missing length stays NULL, never zero: `Progress` must never be handed a zero denominator.
#[tokio::test]
async fn a_pdf_that_will_not_say_leaves_the_length_absent() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("Unreadable.pdf");
    write(&src, &bookmole::pdf::synthetic_pdf(0, None));

    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    let book = engine
        .storage()
        .get_book(report.book_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(book.page_count, None, "a length we could not read is NULL");
}

/// `fill_book`'s stored row wins: a page count already on the book is a provider's claim an
/// attached PDF must not redefine.
#[tokio::test]
async fn attaching_a_pdf_fills_a_gap_and_never_overwrites_an_answer() {
    let (tmp, engine) = engine().await;
    let book = engine
        .save_book(&Book {
            title: Some("Middlemarch".into()),
            page_count: Some(490),
            ..Default::default()
        })
        .await
        .unwrap();
    let id = book.id.unwrap();

    let src = tmp.path().join("scan.pdf");
    write(&src, &bookmole::pdf::synthetic_pdf(512, None));
    engine.add_file_to_book(id, &src).await.unwrap();

    let after = engine.storage().get_book(id).await.unwrap().unwrap();
    assert_eq!(after.page_count, Some(490), "the stored answer wins");
}

#[tokio::test]
async fn a_pdf_titled_after_its_word_document_falls_back_to_the_filename() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("Critique of Pure Reason.pdf");
    write(
        &src,
        &bookmole::pdf::synthetic_pdf(9, Some(b"Microsoft Word - kant_final_v2.doc")),
    );

    let id = engine.identify_file(&src).await.unwrap();
    assert_eq!(id.title.as_deref(), Some("Critique of Pure Reason"));
    assert_eq!(id.page_count, Some(9));
}

#[tokio::test]
async fn identify_reports_a_pdfs_length_without_writing_anything() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("Anna Karenina.pdf");
    write(
        &src,
        &bookmole::pdf::synthetic_pdf(864, Some(b"Anna Karenina")),
    );

    let id = engine.identify_file(&src).await.unwrap();
    assert_eq!(id.page_count, Some(864));
    assert_eq!(id.title.as_deref(), Some("Anna Karenina"));
    assert_eq!(library_size(&engine).await, 0);
    assert!(stored_files(&files_dir(&tmp)).is_empty());
}

#[tokio::test]
async fn an_epub_reports_no_length_because_it_has_none() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("No ISBN Here.epub");
    write_isbnless_epub(&src, "No ISBN Here");
    assert_eq!(engine.identify_file(&src).await.unwrap().page_count, None);
}

/// A `.pdf.part` is deliberately not a pdf, so no PDF reader runs on a file still being written.
#[tokio::test]
async fn the_extension_decides_the_format_and_case_does_not() {
    let (tmp, engine) = engine().await;
    let bytes = bookmole::pdf::synthetic_pdf(4, None);

    let shouty = tmp.path().join("Shouty.PDF");
    write(&shouty, &bytes);
    let id = engine.identify_file(&shouty).await.unwrap();
    assert_eq!(id.format, "pdf");
    assert_eq!(id.page_count, Some(4), "case must not hide the reader");

    let partial = tmp.path().join("Half Downloaded.pdf.part");
    write(&partial, &bytes);
    let id = engine.identify_file(&partial).await.unwrap();
    assert_eq!(id.format, "part");
    assert_eq!(
        id.page_count, None,
        "a partial download is not a pdf, whatever its bytes look like"
    );
}

#[tokio::test]
async fn each_reading_carries_its_own_progress_and_not_the_books() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("Middlemarch.pdf");
    write(&src, &bookmole::pdf::synthetic_pdf(500, None));
    let id = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap()
        .book_id
        .unwrap();

    engine
        .update_progress(id, Some(500), Some(true))
        .await
        .unwrap();
    engine.reread(id).await.unwrap();
    engine.update_progress(id, Some(100), None).await.unwrap();

    let reads = engine.readings_with_progress(id).await.unwrap();
    assert_eq!(reads.len(), 2, "a reread is a row, not a flag");

    assert_eq!(reads[0].1, bookmole::Progress::Finished);
    assert_eq!(reads[1].1.percent(), Some(20));
    assert_eq!(reads[1].1.page(), Some(100));

    let book = engine.storage().get_book(id).await.unwrap().unwrap();
    assert_eq!(bookmole::Progress::of_book(&book).percent(), Some(20));
}

#[tokio::test]
async fn one_read_makes_the_two_progress_forms_agree() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("Anna.pdf");
    write(&src, &bookmole::pdf::synthetic_pdf(200, None));
    let id = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap()
        .book_id
        .unwrap();
    engine.update_progress(id, Some(50), None).await.unwrap();

    let book = engine.storage().get_book(id).await.unwrap().unwrap();
    let reads = engine.readings_with_progress(id).await.unwrap();
    assert_eq!(reads[0].1, bookmole::Progress::of_book(&book));

    let active = engine.active_reading_with_progress(id).await.unwrap();
    assert_eq!(active.unwrap().1, reads[0].1);
    let one = engine.reading_with_progress(reads[0].0.id).await.unwrap();
    assert_eq!(one.unwrap().1, reads[0].1);
}

#[tokio::test]
async fn a_reading_with_no_denominator_reports_a_page_and_no_fraction() {
    let (_tmp, engine) = engine().await;
    let book = engine
        .save_book(&Book {
            title: Some("Unmeasured".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let id = book.id.unwrap();
    engine.update_progress(id, Some(42), None).await.unwrap();

    let p = engine.readings_with_progress(id).await.unwrap()[0].1;
    assert_eq!(p.page(), Some(42));
    assert_eq!(p.percent(), None, "no denominator is no percentage, not 0%");
}

#[tokio::test]
async fn a_pointed_epub_has_a_table_of_contents() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("Pointed Toc.epub");
    std::fs::copy(toc_fixture(), &src).unwrap();
    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    let book_id = report.book_id.unwrap();
    assert!(engine.book_files(book_id).await.unwrap().is_empty());

    let toc = engine.table_of_contents(book_id).await.unwrap().unwrap();
    assert_eq!(toc.sha256, report.sha256);
    assert_eq!(toc.entries.len(), 4);
    assert_eq!(toc.entries[0].label, "One: The Beginning");
}

#[tokio::test]
async fn a_pointed_file_deleted_from_disk_has_no_toc_and_no_error() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("Pointed Toc.epub");
    std::fs::copy(toc_fixture(), &src).unwrap();
    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    let book_id = report.book_id.unwrap();
    let before = engine.copies_for_book(book_id).await.unwrap();

    std::fs::remove_file(&src).unwrap();
    assert!(engine.table_of_contents(book_id).await.unwrap().is_none());

    let after = engine.copies_for_book(book_id).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].have_now, before[0].have_now);
    assert!(after[0].places[0].missing_since.is_some());

    std::fs::copy(toc_fixture(), &src).unwrap();
    assert!(engine.table_of_contents(book_id).await.unwrap().is_some());
    let back = engine.copies_for_book(book_id).await.unwrap();
    assert!(back[0].places[0].missing_since.is_none());
}

#[tokio::test]
async fn removing_a_kept_file_removes_its_store_place_and_leaves_the_path_place() {
    let (tmp, engine) = engine().await;
    let src = tmp.path().join("A Book With No ISBN.epub");
    write_isbnless_epub(&src, "A Book With No ISBN");
    let report = engine
        .import_file(&src, FileImportOptions::default())
        .await
        .unwrap();
    let book_id = report.book_id.unwrap();
    keep(&engine, &src).await;
    assert_eq!(
        engine.copies_for_book(book_id).await.unwrap()[0]
            .places
            .len(),
        2
    );

    assert!(engine.remove_file(&report.sha256).await.unwrap());
    let copies = engine.copies_for_book(book_id).await.unwrap();
    assert_eq!(copies.len(), 1);
    let kinds: Vec<_> = copies[0].places.iter().map(|p| p.kind).collect();
    assert_eq!(kinds, [PlaceKind::Path]);
}
