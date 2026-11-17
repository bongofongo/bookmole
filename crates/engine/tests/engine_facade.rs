//! Tests for the `Engine` facade against a `TempDir` and an in-memory database.

use std::path::PathBuf;

use bookmole::{Book, Engine, NewNoteInput, NoteKind, NoteScope};

mod common;
use common::{book, engine, highlight, seed_book, write_isbnless_epub};

#[tokio::test]
async fn open_creates_its_directories_and_migrates() {
    let (tmp, engine) = engine().await;
    assert!(tmp.path().join("database/images").is_dir());
    assert!(tmp.path().join("database/files").is_dir());
    assert!(tmp.path().join("vault").is_dir());
    assert!(
        engine
            .list_notes(NoteScope::All, None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn save_book_round_trips_and_assigns_an_id() {
    let (_tmp, engine) = engine().await;
    let saved = engine.save_book(&book("Middlemarch")).await.unwrap();
    assert!(saved.id.is_some());
    assert_eq!(saved.title.as_deref(), Some("Middlemarch"));
}

#[tokio::test]
async fn resolve_books_covers_id_isbn_title_and_miss() {
    let (_tmp, engine) = engine().await;
    let saved = engine
        .save_book(&Book {
            isbn_13: Some("9781900000253".into()),
            ..book("Middlemarch")
        })
        .await
        .unwrap();
    let id = saved.id.unwrap();

    let by_id = engine.resolve_books(&id.to_string()).await.unwrap();
    assert_eq!(by_id.len(), 1);
    assert_eq!(by_id[0].id, Some(id));

    for sel in ["9781900000253", "978-1-900000-25-3"] {
        let hit = engine.resolve_books(sel).await.unwrap();
        assert_eq!(hit.len(), 1, "ISBN selector {sel:?} did not resolve");
        assert_eq!(hit[0].id, Some(id));
    }

    let by_title = engine.resolve_books("midd").await.unwrap();
    assert_eq!(by_title.len(), 1);

    assert!(
        engine
            .resolve_books("nothing like this")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_ambiguous_title_returns_every_candidate() {
    let (_tmp, engine) = engine().await;
    engine.save_book(&book("The Long Way")).await.unwrap();
    engine.save_book(&book("The Long Goodbye")).await.unwrap();

    let hits = engine.resolve_books("The Long").await.unwrap();
    assert_eq!(hits.len(), 2, "ambiguity must be reported, not resolved");
}

#[tokio::test]
async fn delete_book_removes_the_row_and_its_cover_file() {
    let (tmp, engine) = engine().await;
    let cover = tmp.path().join("database/images/middlemarch.jpg");
    std::fs::create_dir_all(cover.parent().unwrap()).unwrap();
    std::fs::write(&cover, b"not really a jpeg").unwrap();

    let saved = engine
        .save_book(&Book {
            cover_path: Some(cover.display().to_string()),
            ..book("Middlemarch")
        })
        .await
        .unwrap();

    engine.delete_book(saved.id.unwrap()).await.unwrap();
    assert!(
        engine
            .resolve_books("Middlemarch")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!cover.exists(), "cover file was orphaned on disk");
}

#[tokio::test]
async fn measuring_stored_covers_fills_in_what_a_migration_could_not() {
    let (tmp, engine) = engine().await;
    let images = tmp.path().join("database/images");
    std::fs::create_dir_all(&images).unwrap();

    // A legacy-named cover: the back-fill must measure it without renaming it.
    let legacy = images.join("content");
    std::fs::write(&legacy, red_bordered_png(500, 700)).unwrap();
    let junk = images.join("broken.jpg");
    std::fs::write(&junk, b"not really a jpeg").unwrap();

    let measured = engine
        .save_book(&Book {
            cover_path: Some(legacy.display().to_string()),
            ..book("Middlemarch")
        })
        .await
        .unwrap();
    let unreadable = engine
        .save_book(&Book {
            cover_path: Some(junk.display().to_string()),
            ..book("Piranesi")
        })
        .await
        .unwrap();
    let coverless = engine.save_book(&book("Station Eleven")).await.unwrap();

    assert_eq!(measured.cover_width, None, "the state before the back-fill");
    assert_eq!(engine.measure_stored_covers().await.unwrap(), (1, 1));

    let got = engine
        .get_book(measured.id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.cover_path.as_deref(), legacy.to_str());
    assert_eq!((got.cover_width, got.cover_height), (Some(500), Some(700)));
    assert_eq!(got.cover_accent_rgb(), Some([160, 32, 32]));
    assert_eq!(got.cover_aspect(), Some(500.0 / 700.0));
    let thumb = got.cover_thumb_path.clone().expect("a shelf tier");
    assert!(std::path::Path::new(&thumb).exists());
    assert_eq!(got.shelf_cover_path(), Some(thumb.as_str()));

    for id in [unreadable.id.unwrap(), coverless.id.unwrap()] {
        let b = engine.get_book(id).await.unwrap().unwrap();
        assert_eq!(b.cover_width, None);
        assert_eq!(b.cover_accent, None);
    }
    assert_eq!(engine.measure_stored_covers().await.unwrap(), (0, 1));
}

#[tokio::test]
async fn deleting_one_of_two_books_sharing_a_jacket_leaves_the_file_alone() {
    let (tmp, engine) = engine().await;
    let images = tmp.path().join("database/images");
    std::fs::create_dir_all(&images).unwrap();
    let shared = images.join("shared.png");
    std::fs::write(&shared, red_bordered_png(40, 60)).unwrap();

    let mut ids = Vec::new();
    for title in ["Middlemarch", "Middlemarch (again)"] {
        ids.push(
            engine
                .save_book(&Book {
                    cover_path: Some(shared.display().to_string()),
                    ..book(title)
                })
                .await
                .unwrap()
                .id
                .unwrap(),
        );
    }

    engine.delete_book(ids[0]).await.unwrap();
    assert!(shared.exists(), "the surviving book lost its cover");
    engine.delete_book(ids[1]).await.unwrap();
    assert!(!shared.exists(), "the last holder is gone; so is the file");
}

fn red_bordered_png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        if x < 2 || y < 2 || x + 2 >= w || y + 2 >= h {
            image::Rgb([160, 32, 32])
        } else {
            image::Rgb([250, 250, 250])
        }
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[tokio::test]
async fn deleting_a_book_whose_cover_is_already_gone_is_not_an_error() {
    let (tmp, engine) = engine().await;
    let saved = engine
        .save_book(&Book {
            cover_path: Some(tmp.path().join("nope.jpg").display().to_string()),
            ..book("Middlemarch")
        })
        .await
        .unwrap();
    engine
        .delete_book(saved.id.unwrap())
        .await
        .expect("missing cover must not fail");
}

#[tokio::test]
async fn a_note_survives_a_full_create_edit_reread_delete_cycle() {
    let (tmp, engine) = engine().await;
    let saved = engine.save_book(&book("Middlemarch")).await.unwrap();

    let created = engine
        .create_note(NewNoteInput {
            book_id: saved.id,
            page: Some(42),
            kind: NoteKind::Note,
            title: Some("Opening line".into()),
            body: "We are all of us born in moral stupidity.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        created.file.exists(),
        "note file was not written to the vault"
    );

    let notes = engine
        .list_notes(NoteScope::of_book(saved.id), None)
        .await
        .unwrap();
    assert_eq!(notes.len(), 1);
    let note = &notes[0];

    let body = engine.note_body(note).unwrap();
    assert_eq!(body, "We are all of us born in moral stupidity.");
    assert!(!body.contains("---"), "frontmatter leaked into the body");

    let before = std::fs::read_to_string(&created.file).unwrap();
    let header_before = before.split_once("\n---\n").map(|(h, _)| h.to_string());
    engine
        .update_note_body(note, "Rewritten entirely.")
        .await
        .unwrap();

    let after = std::fs::read_to_string(&created.file).unwrap();
    assert_eq!(
        after.split_once("\n---\n").map(|(h, _)| h.to_string()),
        header_before,
        "frontmatter was not preserved across the edit"
    );
    assert!(after.contains("Rewritten entirely."));
    assert!(after.contains("page: 42"), "the anchor was lost");
    assert_eq!(engine.note_body(note).unwrap(), "Rewritten entirely.");

    assert!(
        !engine
            .search_marks("Rewritten", Some(bookmole::SearchSource::Note), None, 10)
            .await
            .unwrap()
            .is_empty()
    );

    engine.delete_note(note).await.unwrap();
    assert!(!created.file.exists(), "vault file outlived its row");
    assert!(
        engine
            .list_notes(NoteScope::of_book(saved.id), None)
            .await
            .unwrap()
            .is_empty()
    );
    let _ = tmp;
}

#[tokio::test]
async fn deleting_a_note_whose_file_is_already_gone_still_clears_the_row() {
    let (_tmp, engine) = engine().await;
    let saved = engine.save_book(&book("Middlemarch")).await.unwrap();
    let created = engine
        .create_note(NewNoteInput {
            book_id: saved.id,
            body: "some thought".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    std::fs::remove_file(&created.file).unwrap();
    let note = &engine
        .list_notes(NoteScope::of_book(saved.id), None)
        .await
        .unwrap()[0];
    engine
        .delete_note(note)
        .await
        .expect("a missing vault file must not block the row delete");
    assert!(
        engine
            .list_notes(NoteScope::of_book(saved.id), None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn refresh_note_from_disk_reindexes_an_external_edit() {
    let (_tmp, engine) = engine().await;
    let saved = engine.save_book(&book("Middlemarch")).await.unwrap();
    let created = engine
        .create_note(NewNoteInput {
            book_id: saved.id,
            body: "original wording".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let note = engine
        .list_notes(NoteScope::of_book(saved.id), None)
        .await
        .unwrap()
        .remove(0);
    let raw = std::fs::read_to_string(&created.file).unwrap();
    std::fs::write(
        &created.file,
        raw.replace("original wording", "externally rewritten"),
    )
    .unwrap();

    engine.refresh_note_from_disk(&note).await.unwrap();
    assert!(
        !engine
            .search_marks("externally", Some(bookmole::SearchSource::Note), None, 10)
            .await
            .unwrap()
            .is_empty(),
        "external edit never reached the index"
    );
}

#[tokio::test]
async fn a_cold_edit_is_found_by_the_sweep_and_becomes_searchable() {
    let (_tmp, engine) = engine().await;
    let saved = engine.save_book(&book("Middlemarch")).await.unwrap();
    let created = engine
        .create_note(NewNoteInput {
            title: Some("Middlemarch, chapter 4".into()),
            book_id: saved.id,
            body: "Dorothea's dignity under pressure.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        engine
            .search_marks("dignity", Some(bookmole::SearchSource::Note), None, 10)
            .await
            .unwrap()
            .len(),
        1
    );

    let raw = std::fs::read_to_string(&created.file).unwrap();
    std::fs::write(
        &created.file,
        raw.replace(
            "Dorothea's dignity under pressure.",
            "Casaubon's calculation.",
        ),
    )
    .unwrap();
    days_later(&created.file);

    let report = engine.reconcile_vault().await.unwrap();
    assert_eq!(report.checked, 1);
    assert_eq!(report.reindexed, 1);
    assert_eq!(report.absent, 0);

    assert_eq!(
        engine
            .search_marks("calculation", Some(bookmole::SearchSource::Note), None, 10)
            .await
            .unwrap()
            .len(),
        1,
        "the sweep never reached the index"
    );
    assert!(
        engine
            .search_marks("dignity", Some(bookmole::SearchSource::Note), None, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_second_sweep_over_an_unchanged_vault_writes_nothing() {
    let (_tmp, engine) = engine().await;
    let saved = engine.save_book(&book("Middlemarch")).await.unwrap();
    for body in ["first thought", "second thought", "third thought"] {
        engine
            .create_note(NewNoteInput {
                book_id: saved.id,
                body: body.into(),
                ..Default::default()
            })
            .await
            .unwrap();
    }

    let first = engine.reconcile_vault().await.unwrap();
    assert_eq!(first.checked, 3);
    assert_eq!(
        first.reindexed, 0,
        "a note the engine just wrote is not a note that changed"
    );

    let second = engine.reconcile_vault().await.unwrap();
    assert_eq!(second, first);
}

#[tokio::test]
async fn the_sweep_never_deletes_a_note_whose_file_is_missing() {
    let (_tmp, engine) = engine().await;
    let saved = engine.save_book(&book("Middlemarch")).await.unwrap();
    let created = engine
        .create_note(NewNoteInput {
            title: Some("Middlemarch, chapter 9".into()),
            book_id: saved.id,
            body: "Something worth keeping.".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    std::fs::remove_file(&created.file).unwrap();
    let report = engine.reconcile_vault().await.unwrap();
    assert_eq!(report.absent, 1);
    assert_eq!(report.reindexed, 0);

    assert_eq!(
        engine
            .search_marks("keeping", Some(bookmole::SearchSource::Note), None, 10)
            .await
            .unwrap()
            .len(),
        1,
        "an absent file destroyed a note"
    );
    assert!(engine.get_note(created.id).await.unwrap().is_some());
}

/// Pushes `mtime` a minute forward: `notes.last_modified` is whole seconds, so a same-second
/// rewrite would be invisible to the sweep.
fn days_later(path: &std::path::Path) {
    let meta = std::fs::metadata(path).unwrap();
    let ahead = meta.modified().unwrap() + std::time::Duration::from_secs(60);
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(ahead)
        .unwrap();
}

#[tokio::test]
async fn export_flashcards_marks_what_it_exported() {
    let (_tmp, engine) = engine().await;
    let saved = engine.save_book(&book("Middlemarch")).await.unwrap();
    let id = saved.id.unwrap();

    engine
        .storage()
        .insert_flashcard(id, None, "middlemarch", Some("a game"))
        .await
        .unwrap();
    engine
        .storage()
        .insert_flashcard(id, None, "ardent", None)
        .await
        .unwrap();

    assert_eq!(engine.list_flashcards(false).await.unwrap().len(), 2);
    assert_eq!(engine.list_flashcards_for_book(id).await.unwrap().len(), 2);

    let (tsv, count) = engine.export_flashcards(false).await.unwrap();
    assert_eq!(count, 2);
    assert!(tsv.starts_with("#separator:tab"));
    assert!(tsv.contains("middlemarch"));

    assert!(
        engine.list_flashcards(false).await.unwrap().is_empty(),
        "cards were not marked exported"
    );
    assert_eq!(engine.list_flashcards(true).await.unwrap().len(), 2);
    assert_eq!(engine.export_flashcards(false).await.unwrap().1, 0);
}

#[tokio::test]
async fn anki_export_keeps_guids_and_marks_what_went_out() {
    let (_tmp, engine) = engine().await;
    let mut b = book("Middlemarch");
    b.authors = vec!["George Eliot".into()];
    let id = engine.save_book(&b).await.unwrap().id.unwrap();
    let other = engine.save_book(&book("Other")).await.unwrap().id.unwrap();
    for (bk, w) in [(id, "middlemarch"), (id, "ardent"), (other, "mot")] {
        engine
            .storage()
            .insert_flashcard(bk, None, w, Some("ctx"))
            .await
            .unwrap();
    }

    let guids = |text: &str| -> Vec<String> {
        text.lines()
            .skip(6)
            .map(|l| l.rsplit('\t').next().unwrap().to_string())
            .collect()
    };

    let first = engine.export_anki(Some(id), false).await.unwrap();
    assert_eq!(first.count, 2);
    assert!(first.text.starts_with("#separator:tab\n#html:true\n"));
    assert!(
        first
            .text
            .contains("<small>Middlemarch — George Eliot</small>")
    );
    assert!(first.filename.starts_with("bookmole-anki-middlemarch-"));
    assert!(first.filename.ends_with(".txt"));

    assert_eq!(engine.export_anki(Some(id), false).await.unwrap().count, 0);
    let rest = engine.export_anki(None, false).await.unwrap();
    assert_eq!(rest.count, 1);
    assert!(rest.text.contains("bookmole::Other"));

    let again = engine.export_anki(Some(id), true).await.unwrap();
    assert_eq!(again.count, 2);
    assert_eq!(guids(&first.text), guids(&again.text));

    let err = engine.export_anki(Some(9_999), true).await.unwrap_err();
    assert!(matches!(err, bookmole::EngineError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn anki_save_writes_beside_the_vault_and_only_when_something_went_out() {
    let (_tmp, engine) = engine().await;
    let id = engine.save_book(&book("Dune")).await.unwrap().id.unwrap();
    engine
        .storage()
        .insert_flashcard(id, None, "spice", Some("the spice must flow"))
        .await
        .unwrap();

    let saved = engine.save_anki_export(None, false).await.unwrap();
    assert_eq!(saved.count, 1);
    let path = saved.path.expect("a file was written");
    assert_eq!(path.parent().unwrap(), engine.exports_dir());
    assert_eq!(
        engine.exports_dir().parent(),
        engine.vault_dir().parent(),
        "exports/ is the vault's sibling"
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("spice\tthe spice must flow"));

    let none = engine.save_anki_export(None, false).await.unwrap();
    assert_eq!(none.count, 0);
    assert_eq!(none.path, None);

    let twice = engine.save_anki_export(None, true).await.unwrap();
    let thrice = engine.save_anki_export(None, true).await.unwrap();
    assert_ne!(twice.path, thrice.path);
}

#[tokio::test]
async fn a_failed_anki_write_leaves_the_cards_unmarked() {
    let (_tmp, engine) = engine().await;
    let id = engine.save_book(&book("Dune")).await.unwrap().id.unwrap();
    engine
        .storage()
        .insert_flashcard(id, None, "spice", Some("the spice must flow"))
        .await
        .unwrap();

    std::fs::create_dir_all(engine.exports_dir().parent().unwrap()).unwrap();
    std::fs::write(engine.exports_dir(), b"in the way").unwrap();
    assert!(engine.save_anki_export(None, false).await.is_err());

    std::fs::remove_file(engine.exports_dir()).unwrap();
    let saved = engine.save_anki_export(None, false).await.unwrap();
    assert_eq!(saved.count, 1, "the card survived the failed write");
    assert!(saved.path.is_some());
}

#[tokio::test]
async fn a_malformed_isbn_is_rejected_before_any_network_call() {
    let (_tmp, engine) = engine().await;
    let err = engine.lookup_isbn("9781900000254").await.unwrap_err();
    assert!(
        matches!(err, bookmole::EngineError::InvalidIsbn(_)),
        "expected InvalidIsbn, got {err:?}"
    );
}

#[tokio::test]
async fn importing_a_missing_epub_errors_rather_than_panicking() {
    let (_tmp, engine) = engine().await;
    let err = engine
        .import_epub(&PathBuf::from("/definitely/not/here.epub"))
        .await
        .unwrap_err();
    assert!(matches!(err, bookmole::EngineError::Epub(_)), "{err:?}");
}

#[tokio::test]
async fn importing_koreader_from_an_empty_dir_warns_rather_than_failing() {
    let (tmp, engine) = engine().await;
    let empty = tmp.path().join("empty-library");
    std::fs::create_dir_all(&empty).unwrap();

    let report = engine.import_koreader(&empty, false).await.unwrap();
    assert!(report.imported.is_empty());
    assert_eq!(report.warnings.len(), 1);
    assert!(
        report.warnings[0]
            .to_string()
            .starts_with("no KOReader sidecars found under")
    );
}

async fn links_for(engine: &Engine, book_id: i64) -> Vec<String> {
    engine
        .storage()
        .device_links_for_book(book_id)
        .await
        .unwrap()
}

#[tokio::test]
async fn importing_an_epub_records_the_identity_koreader_will_use() {
    let (tmp, engine) = engine().await;
    let path = tmp.path().join("offline.epub");
    write_isbnless_epub(&path, "A Book With No ISBN");

    let saved = engine.import_epub(&path).await.unwrap();
    let md5 = bookmole::partial_md5(&path).unwrap();

    let linked = engine
        .storage()
        .find_book_by_partial_md5(&md5)
        .await
        .unwrap()
        .expect("the imported epub must be reachable by its device identity");
    assert_eq!(linked.id, saved.id);
    assert_eq!(links_for(&engine, saved.id.unwrap()).await, [md5]);
}

#[tokio::test]
async fn re_importing_the_same_epub_writes_one_mapping() {
    let (tmp, engine) = engine().await;
    let path = tmp.path().join("offline.epub");
    write_isbnless_epub(&path, "A Book With No ISBN");

    let first = engine.import_epub(&path).await.unwrap();
    let second = engine.import_epub(&path).await.unwrap();
    assert_ne!(first.id, second.id, "no ISBN means no upsert key");

    let md5 = bookmole::partial_md5(&path).unwrap();
    assert_eq!(links_for(&engine, first.id.unwrap()).await, [md5]);
    assert!(
        links_for(&engine, second.id.unwrap()).await.is_empty(),
        "the mapping must stay on the copy it was first recorded against"
    );
}

#[tokio::test]
async fn an_imported_epub_claims_the_fields_the_file_supplied() {
    let (tmp, engine) = engine().await;
    let path = tmp.path().join("offline.epub");
    write_isbnless_epub(&path, "A Book With No ISBN");

    let saved = engine.import_epub(&path).await.unwrap();
    assert_eq!(saved.title.as_deref(), Some("A Book With No ISBN"));

    let mut claimed: Vec<(String, String)> = engine
        .storage()
        .field_provenance(saved.id.unwrap())
        .await
        .unwrap()
        .into_iter()
        .map(|f| (f.field, f.source))
        .collect();
    claimed.sort();
    assert_eq!(
        claimed,
        vec![
            ("authors".to_string(), "epub".to_string()),
            ("language".to_string(), "epub".to_string()),
            ("title".to_string(), "epub".to_string()),
        ],
        "the file's three fields, and nothing it did not supply"
    );
}

#[tokio::test]
async fn importing_does_not_repoint_a_link_the_user_made() {
    let (tmp, engine) = engine().await;
    let path = tmp.path().join("offline.epub");
    write_isbnless_epub(&path, "A Book With No ISBN");
    let md5 = bookmole::partial_md5(&path).unwrap();

    let chosen = engine
        .save_book(&book("The Copy The User Chose"))
        .await
        .unwrap();
    engine
        .storage()
        .set_device_link(
            &md5,
            chosen.id.unwrap(),
            bookmole::storage::LinkedBy::Manual,
        )
        .await
        .unwrap();

    engine.import_epub(&path).await.unwrap();

    assert_eq!(links_for(&engine, chosen.id.unwrap()).await, [md5.as_str()]);
    let created = engine
        .storage()
        .find_book_by_partial_md5(&md5)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(created.id, chosen.id, "a scan must not repoint");
}

#[tokio::test]
async fn a_highlight_carries_the_reading_it_was_captured_during() {
    let (_tmp, engine) = engine().await;
    let id = seed_book(&engine, "Middlemarch").await;
    let s = engine.storage();

    let first = s
        .record_reading(
            id,
            unix("2020-01-01"),
            unix("2020-02-01"),
            "finished",
            "manual",
        )
        .await
        .unwrap();
    let second = s
        .record_reading(id, unix("2026-01-01"), None, "reading", "manual")
        .await
        .unwrap();

    for h in [
        highlight("from the first read", "2020-01-15 09:00:00"),
        highlight("from the second read", "2026-01-15 09:00:00"),
        highlight("from neither", "2023-06-01 09:00:00"),
    ] {
        s.insert_highlight(id, &h).await.unwrap();
    }
    s.attribute_highlights(id).await.unwrap();

    let hs = engine.list_highlights(id).await.unwrap();
    let by = |text: &str| {
        hs.iter()
            .find(|h| h.text == text)
            .expect("highlight is listed")
            .reading_id
    };
    assert_eq!(by("from the first read"), Some(first));
    assert_eq!(by("from the second read"), Some(second));
    assert_eq!(
        by("from neither"),
        None,
        "an unplaceable highlight is still listed under the book — `book_id` is \
         authoritative, and only the attribution is missing"
    );
}

#[tokio::test]
async fn highlights_for_reading_is_that_read_and_only_that_read() {
    let (_tmp, engine) = engine().await;
    let id = seed_book(&engine, "Middlemarch").await;
    let s = engine.storage();

    let first = s
        .record_reading(
            id,
            unix("2020-01-01"),
            unix("2020-02-01"),
            "finished",
            "manual",
        )
        .await
        .unwrap();
    let second = s
        .record_reading(id, unix("2026-01-01"), None, "reading", "manual")
        .await
        .unwrap();
    for h in [
        highlight("first a", "2020-01-10 09:00:00"),
        highlight("first b", "2020-01-20 09:00:00"),
        highlight("second a", "2026-01-15 09:00:00"),
        highlight("unplaceable", "2023-06-01 09:00:00"),
    ] {
        s.insert_highlight(id, &h).await.unwrap();
    }
    s.attribute_highlights(id).await.unwrap();

    let texts = |hs: Vec<bookmole::Highlight>| {
        let mut t: Vec<String> = hs.into_iter().map(|h| h.text).collect();
        t.sort();
        t
    };
    assert_eq!(
        texts(engine.highlights_for_reading(first).await.unwrap()),
        ["first a", "first b"]
    );
    assert_eq!(
        texts(engine.highlights_for_reading(second).await.unwrap()),
        ["second a"]
    );
    assert_eq!(
        engine.list_highlights(id).await.unwrap().len(),
        4,
        "the book's own list keeps every highlight, placed or not"
    );
}

#[tokio::test]
async fn the_card_passage_is_the_one_the_reader_chose() {
    let (_tmp, engine) = engine().await;
    let id = seed_book(&engine, "Middlemarch").await;
    let s = engine.storage();

    let reading = s
        .record_reading(id, unix("2026-01-01"), None, "reading", "manual")
        .await
        .unwrap();
    for h in [
        highlight("casaubon", "2026-01-02 09:00:00"),
        highlight(
            "We are all of us born in moral stupidity — the longest mark, which a \
             rule would have chosen",
            "2026-01-03 09:00:00",
        ),
        highlight(
            "a middling clause the reader actually loved",
            "2026-01-04 09:00:00",
        ),
    ] {
        s.insert_highlight(id, &h).await.unwrap();
    }
    s.attribute_highlights(id).await.unwrap();

    assert!(
        engine.card_passage(reading).await.unwrap().is_none(),
        "marks alone choose nothing"
    );
    let offered = engine.card_passage_candidates(reading).await.unwrap();
    assert_eq!(offered.len(), 3, "every mark of the read is on offer");

    let loved = offered
        .iter()
        .find(|h| h.text.starts_with("a middling clause"))
        .unwrap();
    engine
        .set_card_passage(reading, Some(loved.id))
        .await
        .unwrap();
    let passage = engine.card_passage(reading).await.unwrap().unwrap();
    assert_eq!(passage.id, loved.id, "the card shows the reader's pick");

    assert_eq!(
        engine.card_passage(reading).await.unwrap().map(|h| h.id),
        Some(passage.id)
    );

    engine.set_card_passage(reading, None).await.unwrap();
    assert!(engine.card_passage(reading).await.unwrap().is_none());
}

#[tokio::test]
async fn a_reading_with_no_marks_has_no_card_passage() {
    let (_tmp, engine) = engine().await;
    let id = seed_book(&engine, "Piranesi").await;
    let reading = engine
        .storage()
        .record_reading(id, unix("2026-01-01"), None, "reading", "manual")
        .await
        .unwrap();

    assert!(engine.card_passage(reading).await.unwrap().is_none());
    assert!(
        engine
            .card_passage_candidates(reading)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        engine
            .highlights_for_reading(reading)
            .await
            .unwrap()
            .is_empty()
    );
}

/// An unattributed mark of the book is an allowed passage; sidecar imports leave those behind.
#[tokio::test]
async fn a_card_refuses_a_passage_from_another_book_or_another_read() {
    use bookmole::EngineError;

    let (_tmp, engine) = engine().await;
    let middlemarch = seed_book(&engine, "Middlemarch").await;
    let piranesi = seed_book(&engine, "Piranesi").await;
    let s = engine.storage();

    let first = s
        .record_reading(
            middlemarch,
            unix("2020-01-01"),
            unix("2020-02-01"),
            "finished",
            "manual",
        )
        .await
        .unwrap();
    let second = s
        .record_reading(middlemarch, unix("2026-01-01"), None, "reading", "manual")
        .await
        .unwrap();
    for h in [
        highlight("marked the first time through", "2020-01-10 09:00:00"),
        highlight("what the second read noticed", "2026-01-15 09:00:00"),
        highlight("an unplaceable mark", "2023-06-01 09:00:00"),
    ] {
        s.insert_highlight(middlemarch, &h).await.unwrap();
    }
    s.insert_highlight(
        piranesi,
        &highlight("the house is valuable", "2026-01-02 09:00:00"),
    )
    .await
    .unwrap();
    s.attribute_highlights(middlemarch).await.unwrap();

    let by_text =
        |hs: &[bookmole::Highlight], t: &str| hs.iter().find(|h| h.text.starts_with(t)).unwrap().id;
    let middlemarch_marks = engine.list_highlights(middlemarch).await.unwrap();
    let piranesi_marks = engine.list_highlights(piranesi).await.unwrap();
    let first_mark = by_text(&middlemarch_marks, "marked the first");
    let second_mark = by_text(&middlemarch_marks, "what the second");
    let loose = by_text(&middlemarch_marks, "an unplaceable");
    let other_book = by_text(&piranesi_marks, "the house");

    let offered: Vec<i64> = engine
        .card_passage_candidates(second)
        .await
        .unwrap()
        .iter()
        .map(|h| h.id)
        .collect();
    assert!(offered.contains(&second_mark) && offered.contains(&loose));
    assert!(!offered.contains(&first_mark) && !offered.contains(&other_book));

    assert!(matches!(
        engine.set_card_passage(second, Some(other_book)).await,
        Err(EngineError::InvalidInput(_))
    ));
    assert!(matches!(
        engine.set_card_passage(second, Some(first_mark)).await,
        Err(EngineError::InvalidInput(_))
    ));
    assert!(matches!(
        engine.set_card_passage(second, Some(999_999)).await,
        Err(EngineError::NotFound(_))
    ));
    assert!(matches!(
        engine.set_card_passage(999_999, Some(second_mark)).await,
        Err(EngineError::NotFound(_))
    ));
    assert!(
        engine.card_passage(second).await.unwrap().is_none(),
        "a refused write writes nothing"
    );

    engine.set_card_passage(second, Some(loose)).await.unwrap();
    engine
        .set_card_passage(first, Some(first_mark))
        .await
        .unwrap();
    assert_eq!(
        engine.card_passage(second).await.unwrap().map(|h| h.id),
        Some(loose)
    );
    assert_eq!(
        engine.card_passage(first).await.unwrap().map(|h| h.id),
        Some(first_mark)
    );
}

fn unix(day: &str) -> Option<i64> {
    bookmole::storage::ko_datetime_to_unix(&format!("{day} 00:00:00"))
}

#[tokio::test]
async fn subjects_and_series_can_be_made_the_users_own() {
    let (_tmp, engine) = engine().await;
    let saved = engine
        .save_book(&Book {
            title: Some("Dune Messiah".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let id = saved.id.unwrap();

    let after = engine
        .set_book_fields(
            id,
            &Book {
                subjects: vec!["Science fiction".into()],
                series: Some("Dune".into()),
                series_index: Some(2.0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(after.series.as_deref(), Some("Dune"));
    assert_eq!(after.series_index, Some(2.0));
    assert_eq!(after.subjects, vec!["Science fiction"]);

    let mine: Vec<String> = engine
        .field_provenance(id)
        .await
        .unwrap()
        .into_iter()
        .filter(|f| f.source == "user")
        .map(|f| f.field)
        .collect();
    for field in ["subjects", "series", "series_index"] {
        assert!(
            mine.iter().any(|f| f == field),
            "{field} is not claimed: {mine:?}"
        );
    }
}
