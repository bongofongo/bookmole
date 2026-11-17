//! Calibre tests driven through fake binaries via `calibre_bin_dir`, not a `PATH` edit
//! (`set_var` races other threads).
#![cfg(unix)]

use std::path::{Path, PathBuf};

use bookmole::calibre::{CalibreMatch, ImportOptions};
use bookmole::{Book, EngineError};

mod common;

const LIBRARY_JSON: &str = include_str!("fixtures/calibre/recorded/library.json");

struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    fn new() -> Fake {
        Fake {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn tool(&self, name: &str, body: &str) -> &Fake {
        use std::os::unix::fs::PermissionsExt;
        let p = self.dir.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).expect("write fake");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        self
    }

    fn calibredb(&self, json: &str) -> &Fake {
        let out = self.dir.path().join("argv.txt");
        self.tool(
            "calibredb",
            &format!(
                "printf '%s\\n' \"$@\" > {argv}\ncat <<'RB_EOF'\n{json}\nRB_EOF",
                argv = out.display()
            ),
        )
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("argv.txt"))
            .expect("the fake ran")
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn library(root: &Path, covers: &[(&str, &[u8])]) -> PathBuf {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("metadata.db"), b"not really sqlite").unwrap();
    for (rel, bytes) in covers {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }
    root.to_path_buf()
}

fn json_rooted(root: &Path) -> String {
    LIBRARY_JSON.replace("/books/calibre", &root.display().to_string())
}

#[tokio::test]
async fn a_conversion_runs_the_tool_and_is_judged_by_what_it_wrote() {
    let fake = Fake::new();
    let work = tempfile::tempdir().unwrap();
    let input = work.path().join("in.epub");
    let output = work.path().join("out.azw3");
    std::fs::write(&input, b"epub").unwrap();

    fake.tool("ebook-convert", "cp \"$1\" \"$2\"");
    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    assert!(engine.calibre().can_convert());

    let written = engine.convert_ebook(&input, &output, false).await.unwrap();
    assert_eq!(written, output);
    assert!(output.is_file());

    let e = engine
        .convert_ebook(&input, &output, false)
        .await
        .unwrap_err();
    assert!(
        matches!(&e, EngineError::InvalidInput(m) if m.contains("already exists")),
        "{e:?}"
    );
    engine.convert_ebook(&input, &output, true).await.unwrap();
}

#[tokio::test]
async fn a_failing_conversion_is_typed_and_keeps_calibres_last_words() {
    let fake = Fake::new();
    let work = tempfile::tempdir().unwrap();
    let input = work.path().join("in.epub");
    std::fs::write(&input, b"epub").unwrap();
    fake.tool(
        "ebook-convert",
        "echo 'Traceback (most recent call last):' >&2\necho 'ValueError: no such format' >&2\nexit 1",
    );

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let e = engine
        .convert_ebook(&input, &work.path().join("out.xyz"), false)
        .await
        .unwrap_err();
    match &e {
        EngineError::Calibre { tool, message } => {
            assert_eq!(tool, "ebook-convert");
            assert!(message.contains("no such format"), "{message}");
        }
        other => panic!("expected a typed calibre failure, got {other:?}"),
    }

    fake.tool("ebook-convert", "exit 0");
    let (_tmp2, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let e = engine
        .convert_ebook(&input, &work.path().join("nothing.azw3"), false)
        .await
        .unwrap_err();
    assert!(matches!(e, EngineError::Calibre { .. }), "{e:?}");
}

// The absent-calibre path is not tested here: detection falls through to the real `PATH`, so an
// installed calibre would pass for the wrong reason.

#[tokio::test]
async fn a_library_import_builds_the_command_line_and_lands_the_books() {
    let work = tempfile::tempdir().unwrap();
    let lib = library(
        &work.path().join("lib"),
        &[
            (
                "Jacob Grimm/Grimms' Fairy Tales (1)/cover.jpg",
                b"jpegbytes",
            ),
            (
                "Jacob Grimm/Grimms' Fairy Tales (1)/Grimms' Fairy Tales - Jacob Grimm.epub",
                b"epub-one",
            ),
            (
                "Jacob Grimm/Grimms' Fairy Tales (1)/Grimms' Fairy Tales - Jacob Grimm.azw3",
                b"azw3-one",
            ),
            (
                "Charlotte Perkins Gilman/The Yellow Wallpaper (2)/The Yellow Wallpaper - Charlotte Perkins Gilman.epub",
                b"epub-two",
            ),
        ],
    );
    let fake = Fake::new();
    fake.calibredb(&json_rooted(&work.path().join("lib")));

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let report = engine
        .import_calibre_library(&ImportOptions {
            library: Some(lib.clone()),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(
        fake.argv(),
        [
            "--with-library",
            lib.to_str().unwrap(),
            "list",
            "--for-machine",
            "--fields",
            "all"
        ],
        "the command line is the whole interface to another program"
    );

    assert_eq!(report.rows, 3);
    assert_eq!(report.books.len(), 3);
    assert_eq!(report.created(), 3);
    assert!(report.unmatched.is_empty());

    let grimm = report
        .books
        .iter()
        .find(|b| b.title == "Grimms' Fairy Tales")
        .unwrap();
    assert_eq!(grimm.matched_by, CalibreMatch::New);
    assert_eq!(grimm.tags_added, 2);
    assert!(grimm.cover, "calibre's cover filled an empty slot");
    assert_eq!(grimm.files_linked, 2, "every format, not just the epub");

    let book = engine
        .storage()
        .get_book(grimm.book_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(book.authors, ["Jacob Grimm", "Wilhelm Grimm"]);
    assert_eq!(book.isbn_13.as_deref(), Some("9780140447101"));
    assert_eq!(book.publish_year, Some(2001));
    assert_eq!(book.language.as_deref(), Some("en"));
    assert_eq!(book.series.as_deref(), Some("Household Tales"));
    assert_eq!(book.series_index, Some(1.0));
    assert!(
        book.subjects.is_empty(),
        "a calibre tag is a minted shelf, never a bibliographic subject"
    );
    let cover = PathBuf::from(book.cover_path.expect("a cover path"));
    assert!(cover.is_file());
    assert!(cover.starts_with(engine.images_dir()));
    assert_eq!(std::fs::read(&cover).unwrap(), b"jpegbytes");

    assert_eq!(
        engine
            .storage()
            .book_for_external_id("calibre", "8810c85d-bb14-4a98-a1a9-897508436610")
            .await
            .unwrap(),
        grimm.book_id
    );
    let tags = engine.storage().book_tags(book.id.unwrap()).await.unwrap();
    assert_eq!(
        tags.iter().map(|t| t.tag.as_str()).collect::<Vec<_>>(),
        ["fairy-tales", "german-lit"]
    );

    let claimed = engine
        .storage()
        .field_provenance(book.id.unwrap())
        .await
        .unwrap();
    assert!(
        claimed.iter().all(|f| f.source == "calibre"),
        "nothing else has written to this book: {claimed:?}"
    );
    let fields: Vec<&str> = claimed.iter().map(|f| f.field.as_str()).collect();
    assert!(fields.contains(&"title") && fields.contains(&"isbn_13"));
    assert!(
        !fields.contains(&"page_count"),
        "calibre said nothing about the page count, so it claims nothing: {fields:?}"
    );

    let wallpaper = report
        .books
        .iter()
        .find(|b| b.title == "The Yellow Wallpaper")
        .unwrap();
    let wallpaper_book = engine
        .storage()
        .get_book(wallpaper.book_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wallpaper_book.publish_year, None);
}

#[tokio::test]
async fn importing_the_same_library_twice_changes_nothing() {
    let work = tempfile::tempdir().unwrap();
    let lib = library(
        &work.path().join("lib"),
        &[
            (
                "Jacob Grimm/Grimms' Fairy Tales (1)/cover.jpg",
                b"jpegbytes",
            ),
            (
                "Jacob Grimm/Grimms' Fairy Tales (1)/Grimms' Fairy Tales - Jacob Grimm.epub",
                b"epub-one",
            ),
        ],
    );
    let fake = Fake::new();
    fake.calibredb(&json_rooted(&work.path().join("lib")));

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let opts = ImportOptions {
        library: Some(lib),
        ..Default::default()
    };
    let first = engine.import_calibre_library(&opts).await.unwrap();
    assert_eq!(first.created(), 3);

    let again = engine.import_calibre_library(&opts).await.unwrap();
    assert_eq!(again.created(), 0, "a second run creates nothing");
    assert!(
        again
            .books
            .iter()
            .all(|b| b.matched_by == CalibreMatch::Uuid),
        "every row found itself by uuid: {:?}",
        again.books.iter().map(|b| b.matched_by).collect::<Vec<_>>()
    );
    for b in &again.books {
        assert_eq!(b.tags_added, 0, "{}: tags", b.title);
        assert!(!b.cover, "{}: cover", b.title);
        assert_eq!(b.files_linked, 0, "{}: files", b.title);
    }
    let first_grimm = first
        .books
        .iter()
        .find(|b| b.title == "Grimms' Fairy Tales")
        .unwrap();
    assert_eq!(first_grimm.tags_added, 2);
    assert!(first_grimm.cover);
    assert_eq!(first_grimm.files_linked, 1);
    assert_eq!(
        engine
            .storage()
            .list_books(&bookmole::BookQuery::new(100, bookmole::BookSort::Title))
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn an_existing_book_is_matched_by_isbn_and_never_duplicated() {
    let work = tempfile::tempdir().unwrap();
    let lib = library(&work.path().join("lib"), &[]);
    let fake = Fake::new();
    fake.calibredb(&json_rooted(&work.path().join("lib")));

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let existing = engine
        .save_book(&Book {
            title: Some("Household Stories (a different paperback)".into()),
            isbn_13: Some("9780140447101".into()),
            page_count: Some(490),
            ..Default::default()
        })
        .await
        .unwrap();

    let report = engine
        .import_calibre_library(&ImportOptions {
            library: Some(lib),
            ..Default::default()
        })
        .await
        .unwrap();
    let matched = report
        .books
        .iter()
        .find(|b| b.book_id == existing.id)
        .unwrap();
    assert_eq!(matched.matched_by, CalibreMatch::Isbn);

    let after = engine
        .storage()
        .get_book(existing.id.unwrap())
        .await
        .unwrap()
        .unwrap();
    // The provider no-clobber merge: `calibredb list` carries no page count, so straight assignment
    // would blank it.
    assert_eq!(
        after.page_count,
        Some(490),
        "calibre must not blank what it does not carry"
    );
    assert_eq!(after.publisher.as_deref(), Some("Penguin Classics"));
}

#[tokio::test]
async fn a_near_miss_is_offered_rather_than_duplicated_unless_asked() {
    let work = tempfile::tempdir().unwrap();
    let lib = library(&work.path().join("lib"), &[]);
    let fake = Fake::new();
    fake.calibredb(&json_rooted(&work.path().join("lib")));

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    // 0.77 similarity: inside the 0.60..0.85 ambiguous band, with no ISBN so the title rung decides.
    engine
        .save_book(&Book {
            title: Some("The Yellow Wallpaper: A Story of a Rest Cure".into()),
            // Stored as the origin spelled it; the row the user reads must not be.
            authors: vec!["Gilman, Charlotte Perkins".into()],
            publish_year: Some(1899),
            ..Default::default()
        })
        .await
        .unwrap();

    let opts = ImportOptions {
        library: Some(lib),
        ..Default::default()
    };
    let report = engine.import_calibre_library(&opts).await.unwrap();
    let offered = report
        .unmatched
        .iter()
        .find(|u| u.title == "The Yellow Wallpaper")
        .expect("the near-miss is reported rather than swallowed");
    assert!(
        !offered.candidates.is_empty(),
        "and it says what it probably is"
    );
    assert_eq!(
        offered.candidates[0].authors_display,
        ["Charlotte Perkins Gilman"]
    );
    assert_eq!(offered.candidates[0].publish_year, Some(1899));
    assert!(offered.uuid.is_some());

    let forced = engine
        .import_calibre_library(&ImportOptions {
            create_ambiguous: true,
            ..opts
        })
        .await
        .unwrap();
    assert!(forced.unmatched.is_empty());
    assert!(
        forced
            .books
            .iter()
            .any(|b| b.title == "The Yellow Wallpaper" && b.matched_by == CalibreMatch::New)
    );
}

#[tokio::test]
async fn a_dry_run_reports_the_same_thing_and_writes_nothing() {
    let work = tempfile::tempdir().unwrap();
    let lib = library(
        &work.path().join("lib"),
        &[(
            "Jacob Grimm/Grimms' Fairy Tales (1)/cover.jpg",
            b"jpegbytes",
        )],
    );
    let fake = Fake::new();
    fake.calibredb(&json_rooted(&work.path().join("lib")));

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let report = engine
        .import_calibre_library(&ImportOptions {
            library: Some(lib),
            dry_run: true,
            ..Default::default()
        })
        .await
        .unwrap();

    assert!(report.dry_run);
    assert_eq!(report.created(), 3);
    assert!(report.books.iter().all(|b| b.book_id.is_none()));
    assert_eq!(
        engine
            .storage()
            .list_books(&bookmole::BookQuery::new(100, bookmole::BookSort::Title))
            .await
            .unwrap()
            .len(),
        0
    );
    let images = engine
        .images_dir()
        .read_dir()
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(images, 0, "a dry run must not copy covers either");
}

/// calibredb creates a library and exits 0 for a mistyped path, so the path is checked first.
#[tokio::test]
async fn a_mistyped_library_path_is_refused_before_calibredb_is_run() {
    let work = tempfile::tempdir().unwrap();
    let typo = work.path().join("Calbire Library");
    std::fs::create_dir_all(&typo).unwrap();
    let fake = Fake::new();
    fake.calibredb("[]");

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let e = engine
        .import_calibre_library(&ImportOptions {
            library: Some(typo),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&e, EngineError::InvalidInput(m) if m.contains("metadata.db")),
        "{e:?}"
    );
    assert!(
        !fake.dir.path().join("argv.txt").exists(),
        "the refusal must come before the program runs, or calibre has already \
         created the library the refusal is about"
    );
}

#[tokio::test]
async fn calibres_files_are_identified_so_the_device_can_find_them_later() {
    let work = tempfile::tempdir().unwrap();
    let lib = library(
        &work.path().join("lib"),
        &[(
            "Jacob Grimm/Grimms' Fairy Tales (1)/Grimms' Fairy Tales - Jacob Grimm.epub",
            b"the bytes koreader would open",
        )],
    );
    let fake = Fake::new();
    fake.calibredb(&json_rooted(&work.path().join("lib")));

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let report = engine
        .import_calibre_library(&ImportOptions {
            library: Some(lib.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    let grimm = report
        .books
        .iter()
        .find(|b| b.title == "Grimms' Fairy Tales")
        .unwrap();
    assert_eq!(grimm.files_linked, 1, "the azw3 is not on disk here");

    let md5 = bookmole::partial_md5(
        &lib.join("Jacob Grimm/Grimms' Fairy Tales (1)/Grimms' Fairy Tales - Jacob Grimm.epub"),
    )
    .unwrap();
    let found = engine
        .storage()
        .find_book_by_partial_md5(&md5)
        .await
        .unwrap()
        .expect("the file is identified");
    assert_eq!(found.id, grimm.book_id);
}

#[tokio::test]
async fn a_titleless_row_is_skipped_and_an_id_less_row_says_it_will_recur() {
    let work = tempfile::tempdir().unwrap();
    let lib = library(&work.path().join("lib"), &[]);
    let fake = Fake::new();
    fake.calibredb(
        r#"[
            {"id": 7, "authors": "Nobody"},
            {"id": 8, "title": "No uuid here"}
        ]"#,
    );

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let report = engine
        .import_calibre_library(&ImportOptions {
            library: Some(lib),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(report.rows, 2);
    assert_eq!(report.books.len(), 1, "the titleless row imported nothing");
    let said: Vec<String> = report.warnings.iter().map(|w| w.to_string()).collect();
    assert!(
        said.iter()
            .any(|w| w.contains("calibre #7") && w.contains("nothing to match on")),
        "{said:?}"
    );
    assert!(
        said.iter()
            .any(|w| w.contains("calibre #8") && w.contains("again on a second run")),
        "{said:?}"
    );
}

#[tokio::test]
async fn only_imports_the_row_it_was_asked_for_and_counts_only_that_row() {
    let work = tempfile::tempdir().unwrap();
    let lib = library(&work.path().join("lib"), &[]);
    let fake = Fake::new();
    fake.calibredb(&json_rooted(&work.path().join("lib")));

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let report = engine
        .import_calibre_library(&ImportOptions {
            library: Some(lib),
            only: vec![2],
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(report.rows, 1, "rows is what this import considered");
    assert_eq!(report.books.len(), 1);
    assert_eq!(report.books[0].calibre_id, 2);
    assert_eq!(report.books[0].title, "The Yellow Wallpaper");
    let shelf = engine
        .list_books(&bookmole::BookQuery::new(100, bookmole::BookSort::Title))
        .await
        .unwrap();
    assert_eq!(shelf.len(), 1, "the other two rows were left alone");
}

#[tokio::test]
async fn an_empty_only_is_the_whole_library() {
    let work = tempfile::tempdir().unwrap();
    let lib = library(&work.path().join("lib"), &[]);
    let fake = Fake::new();
    fake.calibredb(&json_rooted(&work.path().join("lib")));

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let report = engine
        .import_calibre_library(&ImportOptions {
            library: Some(lib),
            only: vec![],
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(report.rows, 3);
    assert_eq!(report.books.len(), 3);
}

#[tokio::test]
async fn every_report_line_names_its_calibre_row() {
    let work = tempfile::tempdir().unwrap();
    let lib = library(&work.path().join("lib"), &[]);
    let fake = Fake::new();
    fake.calibredb(&json_rooted(&work.path().join("lib")));

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    for dry_run in [true, false] {
        let report = engine
            .import_calibre_library(&ImportOptions {
                library: Some(lib.clone()),
                dry_run,
                ..Default::default()
            })
            .await
            .unwrap();
        let mut ids: Vec<i64> = report.books.iter().map(|b| b.calibre_id).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2, 3], "dry_run = {dry_run}");
    }
}

#[tokio::test]
async fn linking_a_calibre_book_by_hand_is_matched_by_uuid_from_then_on() {
    let work = tempfile::tempdir().unwrap();
    let lib = library(&work.path().join("lib"), &[]);
    let fake = Fake::new();
    fake.calibredb(&json_rooted(&work.path().join("lib")));

    let (_tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let mine = engine
        .save_book(&Book {
            title: Some("A Novel About Snow".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    engine
        .link_calibre_book("e87a29fd-c971-4c30-ac0b-caabc783bac4", mine.id.unwrap())
        .await
        .unwrap();

    let report = engine
        .import_calibre_library(&ImportOptions {
            library: Some(lib),
            only: vec![2],
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(report.books[0].matched_by, CalibreMatch::Uuid);
    assert_eq!(report.books[0].book_id, mine.id);
    assert_eq!(
        engine
            .list_books(&bookmole::BookQuery::new(100, bookmole::BookSort::Title))
            .await
            .unwrap()
            .len(),
        1,
        "linking then importing must not make a second copy"
    );
}

#[tokio::test]
async fn a_hand_link_can_be_corrected() {
    let (_tmp, engine) = common::engine_with_calibre(None).await;
    let first = engine.save_book(&common::book("Wrong One")).await.unwrap();
    let second = engine.save_book(&common::book("Right One")).await.unwrap();

    engine
        .link_calibre_book("uuid-abc", first.id.unwrap())
        .await
        .unwrap();
    engine
        .link_calibre_book("uuid-abc", second.id.unwrap())
        .await
        .unwrap();

    assert_eq!(
        engine
            .storage()
            .book_for_external_id("calibre", "uuid-abc")
            .await
            .unwrap(),
        second.id
    );
}

#[tokio::test]
async fn linking_to_a_book_that_is_gone_says_so() {
    let (_tmp, engine) = common::engine_with_calibre(None).await;
    let err = engine
        .link_calibre_book("uuid-abc", 9999)
        .await
        .expect_err("there is no book 9999");
    assert!(matches!(err, EngineError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn a_calibre_book_with_no_uuid_cannot_be_linked() {
    let (_tmp, engine) = common::engine_with_calibre(None).await;
    let mine = engine
        .save_book(&common::book("Grimms' Fairy Tales"))
        .await
        .unwrap();
    let err = engine
        .link_calibre_book("   ", mine.id.unwrap())
        .await
        .expect_err("an empty uuid is not an identity");
    assert!(matches!(err, EngineError::InvalidInput(_)), "{err:?}");
}

const GRIMM_UUID: &str = "8810c85d-bb14-4a98-a1a9-897508436610";
const WALLPAPER_UUID: &str = "e87a29fd-c971-4c30-ac0b-caabc783bac4";

async fn copy_import(
    root: &Path,
) -> (
    Fake,
    tempfile::TempDir,
    bookmole::Engine,
    ImportOptions,
    i64,
) {
    let lib = library(
        &root.join("lib"),
        &[
            (
                "Jacob Grimm/Grimms' Fairy Tales (1)/cover.jpg",
                b"jpegbytes",
            ),
            (
                "Jacob Grimm/Grimms' Fairy Tales (1)/Grimms' Fairy Tales - Jacob Grimm.epub",
                b"epub-one",
            ),
        ],
    );
    let fake = Fake::new();
    fake.calibredb(&json_rooted(&root.join("lib")));
    let (tmp, engine) = common::engine_with_calibre(Some(fake.path())).await;
    let opts = ImportOptions {
        library: Some(lib),
        ..Default::default()
    };
    let report = engine.import_calibre_library(&opts).await.unwrap();
    let grimm = report
        .books
        .iter()
        .find(|b| b.title == "Grimms' Fairy Tales")
        .unwrap();
    let id = grimm.book_id.unwrap();
    (fake, tmp, engine, opts, id)
}

#[tokio::test]
async fn a_calibre_import_makes_a_pointed_copy_and_copies_no_bytes() {
    let work = tempfile::tempdir().unwrap();
    let (_fake, tmp, engine, opts, id) = copy_import(work.path()).await;

    let copies = engine.copies_for_book(id).await.unwrap();
    assert_eq!(copies.len(), 1);
    let c = &copies[0];
    assert_eq!(c.form, Some(bookmole::CopyForm::File));
    assert_eq!(c.tenure, bookmole::Tenure::Own);
    assert_eq!(c.learned_from, bookmole::LearnedFrom::Calibre);
    assert_eq!(c.have_now, None);
    assert_eq!(c.places.len(), 1);
    let p = &c.places[0];
    assert_eq!(p.kind, bookmole::PlaceKind::Calibre);
    assert_eq!(p.reference, GRIMM_UUID);
    let detail = p.detail.as_deref().unwrap();
    assert!(detail.starts_with(opts.library.as_ref().unwrap().to_str().unwrap()));
    assert!(detail.ends_with("azw3,epub"), "{detail}");
    assert!(p.last_seen_at.is_some());
    assert!(engine.book_files(id).await.unwrap().is_empty());
    let store = tmp.path().join("database/files");
    assert!(!store.exists() || std::fs::read_dir(store).unwrap().next().is_none());
    assert!(engine.book_standing(id).await.unwrap().in_library);
}

#[tokio::test]
async fn a_second_calibre_scan_makes_no_second_copy() {
    let work = tempfile::tempdir().unwrap();
    let (_fake, _tmp, engine, opts, id) = copy_import(work.path()).await;
    let before = engine.copies_for_book(id).await.unwrap();
    engine.import_calibre_library(&opts).await.unwrap();
    let after = engine.copies_for_book(id).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, before[0].id);
    assert_eq!(after[0].places.len(), 1);
    assert_eq!(after[0].places[0].missing_since, None);
}

#[tokio::test]
async fn a_calibre_book_that_vanishes_is_marked_missing_and_nothing_more() {
    let work = tempfile::tempdir().unwrap();
    let (fake, _tmp, engine, opts, id) = copy_import(work.path()).await;
    let before = engine.copies_for_book(id).await.unwrap();

    let all: serde_json::Value =
        serde_json::from_str(&json_rooted(&work.path().join("lib"))).unwrap();
    let kept: Vec<_> = all
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["uuid"] == WALLPAPER_UUID)
        .cloned()
        .collect();
    fake.calibredb(&serde_json::to_string(&kept).unwrap());
    engine.import_calibre_library(&opts).await.unwrap();

    let gone = engine.copies_for_book(id).await.unwrap();
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0].id, before[0].id);
    assert_eq!(gone[0].have_now, None, "only the user changes have_now");
    assert!(gone[0].places[0].missing_since.is_some());
    assert!(engine.book_standing(id).await.unwrap().in_library);
    let wallpaper = engine
        .copy_for_place(bookmole::PlaceKind::Calibre, WALLPAPER_UUID)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wallpaper.places[0].missing_since, None);

    fake.calibredb(&json_rooted(&work.path().join("lib")));
    engine.import_calibre_library(&opts).await.unwrap();
    let back = engine.copies_for_book(id).await.unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].places[0].missing_since, None);
}

#[tokio::test]
async fn an_unreachable_library_marks_its_places_missing() {
    let work = tempfile::tempdir().unwrap();
    let (_fake, _tmp, engine, opts, id) = copy_import(work.path()).await;
    std::fs::remove_file(opts.library.as_ref().unwrap().join("metadata.db")).unwrap();

    let err = engine.import_calibre_library(&opts).await.unwrap_err();
    assert!(matches!(err, EngineError::InvalidInput(_)), "{err:?}");
    let copies = engine.copies_for_book(id).await.unwrap();
    assert_eq!(copies.len(), 1);
    assert!(copies[0].places[0].missing_since.is_some());
    assert_eq!(copies[0].have_now, None);
}

#[tokio::test]
async fn a_calibre_import_records_each_formats_path() {
    let work = tempfile::tempdir().unwrap();
    let (_fake, _tmp, engine, opts, id) = copy_import(work.path()).await;
    let lib = opts.library.as_ref().unwrap();
    let book_dir = lib.join("Jacob Grimm/Grimms' Fairy Tales (1)");

    let files = engine
        .storage()
        .place_files(bookmole::PlaceKind::Calibre, GRIMM_UUID)
        .await
        .unwrap();
    let got: Vec<(String, std::path::PathBuf)> =
        files.into_iter().map(|f| (f.format, f.path)).collect();
    assert_eq!(
        got,
        vec![
            (
                "azw3".to_string(),
                book_dir.join("Grimms' Fairy Tales - Jacob Grimm.azw3")
            ),
            (
                "epub".to_string(),
                book_dir.join("Grimms' Fairy Tales - Jacob Grimm.epub")
            ),
        ]
    );

    let rows = engine.copy_files(id).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.kind == bookmole::PlaceKind::Calibre
        && r.calibre_library.as_deref() == Some(lib.to_str().unwrap())));
    let epub = rows
        .iter()
        .find(|r| r.format.as_deref() == Some("epub"))
        .unwrap();
    assert_eq!((epub.on_disk, epub.size), (Some(true), Some(8)));
    let azw3 = rows
        .iter()
        .find(|r| r.format.as_deref() == Some("azw3"))
        .unwrap();
    assert_eq!((azw3.on_disk, azw3.size), (Some(false), None));
}

#[tokio::test]
async fn a_rescan_with_other_formats_replaces_the_recorded_paths() {
    let work = tempfile::tempdir().unwrap();
    let (fake, _tmp, engine, opts, _id) = copy_import(work.path()).await;
    let lib = work.path().join("lib");

    let mut all: serde_json::Value = serde_json::from_str(&json_rooted(&lib)).unwrap();
    for b in all.as_array_mut().unwrap() {
        if b["uuid"] == GRIMM_UUID {
            b["formats"] = serde_json::json!([format!(
                "{}/Jacob Grimm/Grimms' Fairy Tales (1)/Grimms' Fairy Tales (retitled).pdf",
                lib.display()
            )]);
        }
    }
    fake.calibredb(&serde_json::to_string(&all).unwrap());
    engine.import_calibre_library(&opts).await.unwrap();

    let files = engine
        .storage()
        .place_files(bookmole::PlaceKind::Calibre, GRIMM_UUID)
        .await
        .unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].format, "pdf");
    assert_eq!(
        files[0].path,
        lib.join("Jacob Grimm/Grimms' Fairy Tales (1)/Grimms' Fairy Tales (retitled).pdf")
    );
}
