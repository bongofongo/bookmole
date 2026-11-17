use std::path::PathBuf;
use std::sync::Arc;

use bookmole::{Engine, EngineConfig};
use bookmole_api::{
    Api, ApiError, BookDto, BookFilterDto, BookQueryDto, CopyFormDto, DayRangeDto, EntryShowDto,
    EntrySortDto, ErrorCode, GraphEdgeKindDto, GraphScopeDto, LinkDirectionDto, ListSortDto,
    MomentKindDto, NewCopyDto, NewNoteDto, NoteKindDto, Outcome, ReadingFilterDto, ReadingQueryDto,
    ReadingStateDto, Request, Response, SearchHitDto, SearchSourceDto, ShapeSourceDto,
    StatusFilterDto, TenureDto,
};

/// The `TempDir` is returned so the vault outlives the test body.
async fn api() -> (Api, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = EngineConfig::in_memory(tmp.path());
    let engine = Engine::open(config).await.expect("engine");
    (Api::new(Arc::new(engine)), tmp)
}

/// Own a copy of the book; Library listings show only books you have.
async fn have(api: &Api, book_id: i64) {
    api.add_copy(NewCopyDto {
        book_id,
        form: Some(CopyFormDto::Print),
        tenure: TenureDto::Own,
        have_now: None,
        from_whom: None,
        app: None,
        location: None,
        started_at: None,
        due_at: None,
        note: None,
    })
    .await
    .expect("add copy");
}

async fn seed(api: &Api) -> i64 {
    let id = api
        .save_book(BookDto {
            title: Some("Station Eleven".into()),
            authors: vec!["Emily St. John Mandel".into()],
            isbn_13: Some("9781447268963".into()),
            page_count: Some(333),
            ..Default::default()
        })
        .await
        .expect("save")
        .id
        .expect("saved book has an id");
    have(api, id).await;
    id
}

fn ok(response: Result<Response, ApiError>) -> Response {
    response.expect("dispatch succeeded")
}

/// The daemon hands one `Api` to every connection at once, so it must be `Send + Sync`.
#[test]
fn the_api_can_be_shared_across_connections() {
    fn assert_send_sync<T: Send + Sync + Clone + 'static>() {}
    assert_send_sync::<Api>();
}

#[tokio::test]
async fn dispatch_and_the_typed_method_agree() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;

    let typed = api.get_book(id).await.unwrap();
    match ok(api.dispatch(Request::GetBook { id }).await) {
        Response::Book(dispatched) => assert_eq!(dispatched, typed),
        other => panic!("{other:?}"),
    }

    let typed = api
        .list_books(BookQueryDto {
            sort: Default::default(),
            filter: None,
            limit: 10,
            offset: 0,
        })
        .await
        .unwrap();
    match ok(api
        .dispatch(Request::ListBooks {
            limit: 10,
            sort: Default::default(),
            offset: 0,
            filter: None,
        })
        .await)
    {
        Response::Books(dispatched) => assert_eq!(dispatched, typed),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn dispatch_and_the_typed_method_agree_on_a_write() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;

    let typed = api.update_progress(id, Some(40), None).await.unwrap();
    assert_eq!(typed.current_page, Some(40));

    match ok(api
        .dispatch(Request::UpdateProgress {
            book_id: id,
            page: Some(41),
            finished: None,
        })
        .await)
    {
        Response::Book(Some(b)) => assert_eq!(b.current_page, Some(41)),
        other => panic!("{other:?}"),
    }
    assert_eq!(api.list_readings(id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_note_is_written_and_read_back_by_id_alone() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    let created = api
        .create_note(NewNoteDto {
            book_id: Some(book_id),
            body: "A note about the [[Symphony]].".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(
        api.note_body(created.id).await.unwrap(),
        "A note about the [[Symphony]]."
    );
    api.update_note_body(created.id, "Rewritten, still about the [[Symphony]].")
        .await
        .unwrap();
    assert!(
        api.note_body(created.id)
            .await
            .unwrap()
            .starts_with("Rewritten")
    );

    let links = api.outgoing_links(created.id).await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].target_title, "Symphony");
    assert!(links[0].note.is_none());

    api.delete_note(created.id).await.unwrap();
    assert!(api.get_note(created.id).await.unwrap().is_none());
}

/// The stamp is second-resolution, so the test asserts it never goes back, not that it moves.
#[tokio::test]
async fn a_notes_last_kept_moment_crosses_the_seam() {
    let (api, _tmp) = api().await;
    let created = api
        .create_note(NewNoteDto {
            body: "First words.".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let fresh = api.get_note(created.id).await.unwrap().expect("the note");
    let kept = fresh.last_modified.expect("stamped on create");
    assert_eq!(Some(kept), fresh.created_at);

    api.update_note_body(created.id, "Second words.")
        .await
        .unwrap();
    let saved = api.get_note(created.id).await.unwrap().expect("the note");
    assert!(saved.last_modified.expect("still stamped") >= kept);

    match ok(api.dispatch(Request::GetNote { id: created.id }).await) {
        Response::Note(Some(n)) => assert_eq!(n.last_modified, saved.last_modified),
        other => panic!("expected a note, got {other:?}"),
    }
}

#[tokio::test]
async fn an_absent_note_is_not_found_and_not_something_vaguer() {
    let (api, _tmp) = api().await;
    let err = api.note_body(4242).await.expect_err("no such note");
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(err.message.contains("4242"), "{}", err.message);
}

#[tokio::test]
async fn an_absent_calibre_is_a_status_and_never_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let empty = tmp.path().join("no-binaries-here");
    std::fs::create_dir_all(&empty).unwrap();
    let mut config = EngineConfig::in_memory(tmp.path());
    config.calibre_bin_dir = Some(empty);
    let api = Api::new(Arc::new(Engine::open(config).await.unwrap()));

    match ok(api.dispatch(Request::CalibreStatus).await) {
        Response::CalibreStatus(_) => {}
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn an_unmapped_rating_is_a_code_not_a_rounded_star() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    let review = api.open_review(book_id, None).await.unwrap();
    api.set_rating(review.id, 4.25).await.unwrap();

    let stored = api.review_rating(review.id).await.unwrap().expect("rated");
    assert_eq!(stored.value, 4.25);
    assert_eq!(stored.scale.step, 0.25);
    assert_eq!(api.goodreads_rating(review.id).await.unwrap(), Some(4.25));

    api.put_rating_scale("tens", 0.0, 10.0, 1.0).await.unwrap();
    api.set_rating(review.id, 7.0).await.unwrap();
    let err = api
        .goodreads_rating(review.id)
        .await
        .expect_err("7 of 10 has no Goodreads mapping");
    assert_eq!(err.code, ErrorCode::UnmappedRating);
}

#[tokio::test]
async fn opening_a_reflection_twice_opens_the_same_note() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    let first = api.open_reflection(book_id, None).await.unwrap();
    let again = api.open_reflection(book_id, None).await.unwrap();
    assert_eq!(first.id, again.id);
    assert_eq!(first.file, again.file);

    let reading = api.active_reading(book_id).await.unwrap().expect("open");
    let found = api
        .note_for_reading(reading.id, NoteKindDto::Reflection)
        .await
        .unwrap()
        .expect("the reflection");
    assert_eq!(found.id, first.id);
    let review = api.open_review(book_id, None).await.unwrap();
    assert_ne!(review.id, first.id);
}

#[tokio::test]
async fn a_key_can_be_set_but_never_read_back() {
    let (api, _tmp) = api().await;
    assert!(!api.has_google_api_key());

    api.set_google_api_key(Some("AIzaSUPERSECRET".into()));
    assert!(api.has_google_api_key());

    let response = ok(api.dispatch(Request::GoogleApiKey).await);
    let json = serde_json::to_string(&response).unwrap();
    assert!(!json.contains("SUPERSECRET"), "{json}");
    assert_eq!(response, Response::Bool(true));

    api.set_google_api_key(None);
    assert!(!api.has_google_api_key());
}

#[tokio::test]
async fn the_paths_are_the_engines_own() {
    let (api, tmp) = api().await;
    let paths = api.paths();
    assert_eq!(PathBuf::from(&paths.vault_dir), tmp.path().join("vault"));
    assert_eq!(
        PathBuf::from(&paths.files_dir),
        tmp.path().join("database/files")
    );
    assert_eq!(paths.db_url, "sqlite::memory:");
}

#[tokio::test]
async fn an_in_memory_library_is_in_no_folder() {
    let (api, _tmp) = api().await;
    let typed = api.library_location();
    assert_eq!(typed.database_dir, None);
    assert_eq!(typed.cloud_folder, None);
    assert_eq!(
        ok(api.dispatch(Request::LibraryLocation).await),
        Response::LibraryLocation(typed)
    );
}

#[tokio::test]
async fn a_library_on_disk_names_its_database_folder() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let engine = Engine::open(EngineConfig::rooted_at(tmp.path()))
        .await
        .expect("engine");
    let api = Api::new(Arc::new(engine));
    let got = api.library_location();
    assert_eq!(
        got.database_dir.map(PathBuf::from),
        Some(tmp.path().join("database"))
    );
    assert_eq!(got.cloud_folder, None);
}

/// The method is its bare name, an absent fact is `null`, and iCloud Drive is spelled
/// `icloud_drive`.
#[test]
fn the_library_location_reads_plainly_on_the_wire() {
    use bookmole_api::{CloudFolderDto, CloudServiceDto, LibraryLocationDto};
    let req: Request = serde_json::from_str(r#"{"method":"library_location"}"#).unwrap();
    assert_eq!(req, Request::LibraryLocation);

    let none = serde_json::to_value(LibraryLocationDto {
        database_dir: Some("/x/database".into()),
        cloud_folder: None,
    })
    .unwrap();
    assert_eq!(
        none,
        serde_json::json!({ "database_dir": "/x/database", "cloud_folder": null })
    );

    let some = serde_json::to_value(Response::LibraryLocation(LibraryLocationDto {
        database_dir: Some("/Users/u/Library/Mobile Documents/x/database".into()),
        cloud_folder: Some(CloudFolderDto {
            service: CloudServiceDto::ICloudDrive,
            root: "/Users/u/Library/Mobile Documents".into(),
        }),
    }))
    .unwrap();
    assert_eq!(some["shape"], "library_location");
    assert_eq!(some["value"]["cloud_folder"]["service"], "icloud_drive");
    for (dto, wire) in [
        (CloudServiceDto::Dropbox, "dropbox"),
        (CloudServiceDto::GoogleDrive, "google_drive"),
        (CloudServiceDto::OneDrive, "one_drive"),
        (CloudServiceDto::ProtonDrive, "proton_drive"),
        (CloudServiceDto::Box, "box"),
        (CloudServiceDto::CloudStorage, "cloud_storage"),
    ] {
        assert_eq!(serde_json::to_value(dto).unwrap(), wire);
    }
}

#[tokio::test]
async fn a_folder_expands_to_its_book_files_through_both_doors() {
    let (api, _tmp) = api().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    for rel in [
        "b.epub",
        "a/one.pdf",
        "a/cover.jpg",
        ".hidden.epub",
        "Calibre Library/metadata.db",
        "Calibre Library/Author/Title/book.epub",
    ] {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"x").unwrap();
    }
    let paths = vec![root.to_string_lossy().into_owned(), "/not/here.docx".into()];
    let typed = api.expand_book_paths(&paths).await.expect("expands");
    let s = |rel: &str| root.join(rel).to_string_lossy().into_owned();
    assert_eq!(
        typed.files,
        [s("a/one.pdf"), s("b.epub"), "/not/here.docx".into()]
    );
    assert_eq!(typed.calibre_libraries, [s("Calibre Library")]);
    assert_eq!(typed.skipped.hidden, 1);
    assert_eq!(typed.skipped.not_books, 1);
    assert_eq!(typed.cut_off_at, None);
    assert_eq!(
        ok(api.dispatch(Request::ExpandBookPaths { paths }).await),
        Response::ExpandedPaths(typed.clone())
    );

    let wire = serde_json::to_value(Response::ExpandedPaths(typed)).unwrap();
    assert_eq!(wire["shape"], "expanded_paths");
    assert_eq!(wire["value"]["cut_off_at"], serde_json::Value::Null);
    assert_eq!(wire["value"]["skipped"]["not_books"], 1);
    let req: Request =
        serde_json::from_str(r#"{"method":"expand_book_paths","params":{"paths":["/x"]}}"#)
            .unwrap();
    assert_eq!(
        req,
        Request::ExpandBookPaths {
            paths: vec!["/x".into()]
        }
    );
}

#[test]
fn the_vocabulary_survives_json() {
    let calls = vec![
        Request::ApiVersion,
        Request::LibraryLocation,
        Request::CandidateMounts,
        Request::GetBook { id: 3 },
        Request::SaveBook {
            book: BookDto {
                title: Some("t".into()),
                ..Default::default()
            },
        },
        Request::CreateNote {
            note: NewNoteDto {
                book_id: Some(1),
                kind: NoteKindDto::Reflection,
                body: "a body\nwith a newline".into(),
                ..Default::default()
            },
        },
        Request::SyncDevice {
            paths: vec!["/Volumes/KOBOeReader/a.sdr".into()],
        },
        Request::MapRating {
            scale_id: 1,
            value: 4.5,
            goodreads: 4.75,
        },
    ];
    for call in calls {
        let json = serde_json::to_string(&call).unwrap();
        assert_eq!(
            serde_json::from_str::<Request>(&json).unwrap(),
            call,
            "{json}"
        );
    }
}

#[tokio::test]
async fn a_failing_call_still_produces_a_reply() {
    let (api, _tmp) = api().await;
    let reply = api
        .call(bookmole_api::Call {
            id: 99,
            request: Request::GetReading { id: -1 },
        })
        .await;
    assert_eq!(reply.id, 99);
    assert!(matches!(reply.outcome, Outcome::Ok { .. }));

    let reply = api
        .call(bookmole_api::Call {
            id: 100,
            request: Request::DeleteBook { id: -1 },
        })
        .await;
    assert_eq!(reply.id, 100);
}

#[tokio::test]
async fn dispatch_and_the_typed_method_agree_on_linking_a_foreign_record() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;

    api.link_goodreads_row("9000001", id)
        .await
        .expect("typed link");
    match ok(api
        .dispatch(Request::LinkCalibreBook {
            uuid: "c47437a8".into(),
            book_id: id,
        })
        .await)
    {
        Response::Unit => {}
        other => panic!("{other:?}"),
    }

    // The table is keyed `(source, external_id)`; one call must not overwrite the other.
    for (source, external) in [("goodreads", "9000001"), ("calibre", "c47437a8")] {
        let linked = api
            .dispatch(Request::LinkCalibreBook {
                uuid: external.into(),
                book_id: id,
            })
            .await;
        assert!(linked.is_ok(), "{source} link is re-recordable");
    }
}

#[tokio::test]
async fn linking_to_a_missing_book_is_a_typed_error() {
    let (api, _tmp) = api().await;
    let err = api
        .link_calibre_book("uuid-abc", -1)
        .await
        .expect_err("there is no book -1");
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[test]
fn an_import_request_without_only_still_means_the_whole_library() {
    let parsed: Request =
        serde_json::from_str(r#"{"method":"import_calibre_library","params":{"dry_run":true}}"#)
            .expect("an older client's request still parses");
    match parsed {
        Request::ImportCalibreLibrary {
            dry_run,
            only,
            library,
            create_ambiguous,
        } => {
            assert!(dry_run);
            assert!(only.is_empty(), "absent means the whole library");
            assert!(library.is_none());
            assert!(!create_ambiguous);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_period_with_no_device_data_has_no_minutes_rather_than_zero() {
    let (api, _tmp) = api().await;
    seed(&api).await;

    let typed = api
        .activity_summary("2026-01-01", "2026-12-31")
        .await
        .unwrap();
    assert_eq!(typed.minutes, None, "no device has ever spoken here");
    assert_eq!(typed.pages, None);
    assert_eq!(typed.books_finished, 0);
    assert_eq!(typed.range.from, "2026-01-01");

    let json = serde_json::to_string(&typed).unwrap();
    assert!(json.contains("\"minutes\":null"), "{json}");

    match ok(api
        .dispatch(Request::ActivitySummary {
            from: "2026-01-01".into(),
            to: "2026-12-31".into(),
        })
        .await)
    {
        Response::ActivitySummary(d) => assert_eq!(d, typed),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_backwards_range_is_refused_rather_than_answered() {
    let (api, _tmp) = api().await;

    let err = api
        .activity_summary("2026-12-31", "2026-01-01")
        .await
        .expect_err("an inverted range is not a question with an answer");
    assert_eq!(err.code, ErrorCode::InvalidInput);

    match api
        .dispatch(Request::ActivityByDay {
            from: "2026-13-45".into(),
            to: "2026-12-31".into(),
        })
        .await
    {
        Err(e) => assert_eq!(e.code, ErrorCode::InvalidInput),
        Ok(r) => panic!("a non-day was answered: {r:?}"),
    }
}

#[tokio::test]
async fn a_month_with_no_device_data_crosses_the_wire_as_null_minutes() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.create_note(NewNoteDto {
        book_id: Some(book_id),
        body: "A month that happened, and was never timed.".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    api.refill_reading_events().await.unwrap();

    let typed = api
        .activity_by_month("2000-01-01", "2099-12-31")
        .await
        .unwrap();
    assert_eq!(typed.len(), 1, "one month carried an event: {typed:?}");
    let month = &typed[0];
    assert_eq!(month.month.len(), 7, "YYYY-MM: {:?}", month.month);
    assert_eq!(month.activity_days, 1, "the vault knows you were here");
    assert_eq!(month.books, 1);
    assert_eq!(
        month.minutes, None,
        "nothing measured minutes, and zero would be a claim"
    );
    assert_ne!(month.minutes, Some(0));
    assert_eq!(month.pages, None);

    let json = serde_json::to_string(month).unwrap();
    assert!(json.contains("\"minutes\":null"), "{json}");

    match ok(api
        .dispatch(Request::ActivityByMonth {
            from: "2000-01-01".into(),
            to: "2099-12-31".into(),
        })
        .await)
    {
        Response::ActivityByMonth(d) => assert_eq!(d, typed),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_backwards_range_is_refused_by_the_month_aggregate_too() {
    let (api, _tmp) = api().await;

    let err = api
        .activity_by_month("2026-12-31", "2026-01-01")
        .await
        .expect_err("an inverted range is not a question with an answer");
    assert_eq!(err.code, ErrorCode::InvalidInput);

    match api
        .dispatch(Request::ActivityByMonth {
            from: "2026-01-01".into(),
            to: "2026-13-45".into(),
        })
        .await
    {
        Err(e) => assert_eq!(e.code, ErrorCode::InvalidInput),
        Ok(r) => panic!("a non-day was answered: {r:?}"),
    }
}

#[tokio::test]
async fn a_reading_with_no_marks_has_no_card_passage_across_the_seam() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.update_progress(book_id, Some(12), None).await.unwrap();
    let reading = api
        .active_reading(book_id)
        .await
        .unwrap()
        .expect("typing a page opens a reading");

    let typed = api.card_passage(reading.id).await.unwrap();
    assert_eq!(typed, None, "no marks is an answer, not an error");

    match ok(api
        .dispatch(Request::CardPassage {
            reading_id: reading.id,
        })
        .await)
    {
        Response::Highlight(h) => assert_eq!(h, typed),
        other => panic!("{other:?}"),
    }

    assert!(
        api.highlights_for_reading(reading.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn refilling_the_log_is_idempotent_across_the_seam() {
    let (api, _tmp) = api().await;
    seed(&api).await;

    api.refill_reading_events().await.unwrap();
    match ok(api.dispatch(Request::RefillReadingEvents).await) {
        Response::RefillReport(r) => {
            assert_eq!(r.highlights.inserted, 0);
            assert_eq!(r.notes.updated, 0);
            assert_eq!(r.readings.inserted, 0);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_book_with_no_file_has_no_chapter_list_to_read() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;

    assert_eq!(api.table_of_contents(id).await.unwrap(), None);
    match ok(api.dispatch(Request::TableOfContents { book_id: id }).await) {
        Response::TableOfContents(t) => assert_eq!(t, None),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_correction_crosses_as_the_users_and_is_recorded_that_way() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;

    let after = ok(api
        .dispatch(Request::SetBookFields {
            book_id: id,
            fields: BookDto {
                subjects: vec!["Fiction / Dystopian".into()],
                series: Some("Dune".into()),
                series_index: Some(2.0),
                ..Default::default()
            },
        })
        .await);
    match after {
        Response::Book(Some(b)) => {
            assert_eq!(b.subjects, ["Fiction / Dystopian"]);
            assert_eq!(b.series.as_deref(), Some("Dune"));
            assert_eq!(b.series_index, Some(2.0));
        }
        other => panic!("{other:?}"),
    }

    let provenance = api.field_provenance(id).await.unwrap();
    let claimed: Vec<&str> = provenance
        .iter()
        .filter(|f| f.source == "user")
        .map(|f| f.field.as_str())
        .collect();
    assert!(claimed.contains(&"subjects"), "{provenance:?}");
    assert!(claimed.contains(&"series"), "{provenance:?}");
    assert!(claimed.contains(&"series_index"), "{provenance:?}");
    assert!(!provenance.iter().any(|f| f.field == "publisher"));
}

#[test]
fn a_list_books_payload_without_the_new_fields_still_parses() {
    let r: Request =
        serde_json::from_str(r#"{"method":"list_books","params":{"limit":20}}"#).unwrap();
    assert_eq!(
        r,
        Request::ListBooks {
            limit: 20,
            sort: Default::default(),
            offset: 0,
            filter: None,
        }
    );
    let r: Request = serde_json::from_str(r#"{"method":"list_notes","params":{}}"#).unwrap();
    assert_eq!(
        r,
        Request::ListNotes {
            book_id: None,
            reading_id: None,
            limit: None,
        },
        "no limit is every note, which is what this method always did — and a
         payload written before `reading_id` existed still parses, which is why it
         is `#[serde(default)]` and `API_VERSION` did not move"
    );
}

#[tokio::test]
async fn a_filtered_page_and_its_count_agree_over_the_wire() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;
    let second = api
        .save_book(BookDto {
            title: Some("Sea of Tranquility".into()),
            authors: vec!["Emily St. John Mandel".into()],
            isbn_13: Some("9780593321447".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    have(&api, second.id.unwrap()).await;
    api.update_progress(id, Some(40), None).await.unwrap();

    let reading = BookFilterDto {
        status: Some(StatusFilterDto::State {
            is: ReadingStateDto::Reading,
        }),
        ..Default::default()
    };
    match ok(api
        .dispatch(Request::CountBooks {
            filter: Some(reading.clone()),
        })
        .await)
    {
        Response::Count(n) => assert_eq!(n, 1),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ListBooks {
            limit: -1,
            sort: Default::default(),
            offset: 0,
            filter: Some(reading),
        })
        .await)
    {
        Response::Books(books) => assert_eq!(books.len(), 1),
        other => panic!("{other:?}"),
    }

    match ok(api
        .dispatch(Request::CountBooks {
            filter: Some(BookFilterDto {
                status: Some(StatusFilterDto::NoReading),
                ..Default::default()
            }),
        })
        .await)
    {
        Response::Count(n) => assert_eq!(n, 1),
        other => panic!("{other:?}"),
    }
    match ok(api.dispatch(Request::CountBooks { filter: None }).await) {
        Response::Count(n) => assert_eq!(n, 2, "no filter is every book"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_summary_crosses_the_seam_in_the_order_asked() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;
    match ok(api
        .dispatch(Request::BookSummaries {
            book_ids: vec![id, id],
        })
        .await)
    {
        Response::BookSummaries(rows) => {
            assert_eq!(rows.len(), 2);
            assert!(rows.iter().all(|r| r.book_id == id));
            assert_eq!(rows[0].highlights, 0);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_book_carries_the_shape_of_its_edition() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;
    let book = api.get_book(id).await.unwrap().unwrap();
    assert_eq!(book.shape.thickness_source, ShapeSourceDto::Recorded);
    assert_eq!(book.shape.width_source, ShapeSourceDto::Assumed);
    assert!(book.shape.width_over_height > 0.0);
    assert!(book.shape.thickness_over_height > 0.0);

    let bare = BookDto::default();
    assert_eq!(bare.shape.width_source, ShapeSourceDto::Assumed);
    assert_eq!(bare.shape.thickness_source, ShapeSourceDto::Assumed);
}

#[test]
fn a_search_payload_without_a_source_asks_both_indexes() {
    let r: Request =
        serde_json::from_str(r#"{"method":"search_marks","params":{"query":"grief","limit":25}}"#)
            .unwrap();
    assert_eq!(
        r,
        Request::SearchMarks {
            query: "grief".into(),
            source: None,
            book_id: None,
            limit: 25,
        }
    );
    let narrowed: Request = serde_json::from_str(
        r#"{"method":"search_marks","params":{"query":"grief","source":"highlight","limit":5}}"#,
    )
    .unwrap();
    assert_eq!(
        narrowed,
        Request::SearchMarks {
            query: "grief".into(),
            source: Some(SearchSourceDto::Highlight),
            book_id: None,
            limit: 5,
        }
    );
}

#[test]
fn a_payload_written_before_the_scope_existed_still_parses() {
    let old_search: Request =
        serde_json::from_str(r#"{"method":"search_marks","params":{"query":"grief","limit":25}}"#)
            .unwrap();
    assert_eq!(
        old_search,
        Request::SearchMarks {
            query: "grief".into(),
            source: None,
            book_id: None,
            limit: 25,
        },
        "absent is the whole library, exactly as before"
    );
    let old_notes: Request =
        serde_json::from_str(r#"{"method":"list_notes","params":{"book_id":3}}"#).unwrap();
    assert_eq!(
        old_notes,
        Request::ListNotes {
            book_id: Some(3),
            reading_id: None,
            limit: None,
        }
    );

    let scoped: Request = serde_json::from_str(
        r#"{"method":"search_marks","params":{"query":"grief","book_id":7,"limit":25}}"#,
    )
    .unwrap();
    assert_eq!(
        scoped,
        Request::SearchMarks {
            query: "grief".into(),
            source: None,
            book_id: Some(7),
            limit: 25,
        }
    );
    let per_read: Request =
        serde_json::from_str(r#"{"method":"list_notes","params":{"reading_id":2,"limit":12}}"#)
            .unwrap();
    assert_eq!(
        per_read,
        Request::ListNotes {
            book_id: None,
            reading_id: Some(2),
            limit: Some(12),
        }
    );
}

#[tokio::test]
async fn a_scoped_search_crosses_the_seam_without_being_truncated() {
    const LIMIT: i64 = 5;
    let (api, _tmp) = api().await;
    for i in 0..12 {
        let decoy = api
            .save_book(BookDto {
                title: Some(format!("Decoy {i}")),
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap();
        api.create_note(NewNoteDto {
            book_id: Some(decoy),
            title: Some(format!("d{i}")),
            body: "grief".into(),
            kind: NoteKindDto::Note,
            ..Default::default()
        })
        .await
        .unwrap();
    }
    let wanted = seed(&api).await;
    api.create_note(NewNoteDto {
        book_id: Some(wanted),
        title: Some("On collapse".into()),
        body: "a long paragraph about what the collapse does to grief".into(),
        kind: NoteKindDto::Note,
        ..Default::default()
    })
    .await
    .unwrap();

    let unbounded = api.search_marks("grief", None, None, 100).await.unwrap();
    let buried = unbounded
        .iter()
        .position(|h| matches!(h, SearchHitDto::Note { note, .. } if note.book_id == Some(wanted)))
        .expect("the wanted book matches at all");
    assert!(buried >= LIMIT as usize, "fixture ranks it at {buried}");

    let truncated = api.search_marks("grief", None, None, LIMIT).await.unwrap();
    assert_eq!(truncated.len(), LIMIT as usize);
    assert!(
        !truncated
            .iter()
            .any(|h| matches!(h, SearchHitDto::Note { note, .. } if note.book_id == Some(wanted))),
        "nothing for a client-side filter to keep"
    );

    let scoped = api
        .search_marks("grief", None, Some(wanted), LIMIT)
        .await
        .unwrap();
    assert_eq!(scoped.len(), 1);

    match ok(api
        .dispatch(Request::SearchMarks {
            query: "grief".into(),
            source: None,
            book_id: Some(wanted),
            limit: LIMIT,
        })
        .await)
    {
        Response::SearchHits(dispatched) => assert_eq!(dispatched, scoped),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn asking_a_book_and_a_reading_at_once_is_refused_rather_than_guessed() {
    let (api, _tmp) = api().await;
    let book = seed(&api).await;
    let reading = api.reread(book).await.unwrap();

    assert!(api.list_notes(Some(book), None, None).await.is_ok());
    assert!(api.list_notes(None, Some(reading), None).await.is_ok());

    let err = api
        .list_notes(Some(book), Some(reading), None)
        .await
        .expect_err("the pair is not a conjunction");
    assert_eq!(err.code, ErrorCode::InvalidInput);
    assert!(err.message.contains("reading_id"), "{}", err.message);
}

#[test]
fn the_replaced_method_is_gone_from_the_wire() {
    let old = r#"{"method":"search_notes","params":{"query":"grief","limit":25}}"#;
    assert!(
        serde_json::from_str::<Request>(old).is_err(),
        "search_notes still parses, so nothing was actually replaced"
    );
    assert_eq!(bookmole_api::API_VERSION, 2);
}

#[test]
fn a_hit_names_its_kind_on_the_wire() {
    let raw = r#"{"kind":"highlight",
                  "highlight":{"id":7,"book_id":1,"text":"born in moral stupidity",
                               "source":"koreader","created_at":0},
                  "snippet":"born in >>moral<< stupidity"}"#;
    let hit: SearchHitDto = serde_json::from_str(raw).unwrap();
    let SearchHitDto::Highlight { highlight, snippet } = &hit else {
        panic!("the tag did not select the highlight arm: {hit:?}");
    };
    assert_eq!(highlight.id, 7);
    assert_eq!(snippet, "born in >>moral<< stupidity");

    let json = serde_json::to_value(&hit).unwrap();
    assert_eq!(json["kind"], "highlight");
    assert!(
        json.get("note").is_none(),
        "one row, not two nullable ones: {json}"
    );
    assert_eq!(serde_json::from_value::<SearchHitDto>(json).unwrap(), hit);
}

#[tokio::test]
async fn dispatch_and_the_typed_method_agree_on_a_search() {
    let (api, _tmp) = api().await;
    let book = seed(&api).await;
    api.create_note(NewNoteDto {
        book_id: Some(book),
        title: Some("On collapse".into()),
        body: "The symphony rehearses in an antechamber.".into(),
        kind: NoteKindDto::Note,
        ..Default::default()
    })
    .await
    .unwrap();

    let typed = api
        .search_marks("antechamber", None, None, 10)
        .await
        .unwrap();
    assert_eq!(typed.len(), 1);
    assert!(matches!(typed[0], SearchHitDto::Note { .. }));

    match ok(api
        .dispatch(Request::SearchMarks {
            query: "antechamber".into(),
            source: None,
            book_id: None,
            limit: 10,
        })
        .await)
    {
        Response::SearchHits(dispatched) => assert_eq!(dispatched, typed),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn an_empty_answer_is_not_an_error_across_the_seam() {
    let (api, _tmp) = api().await;
    seed(&api).await;
    for query in ["thermodynamics", "", "   ", "don't"] {
        assert!(
            api.search_marks(query, None, None, 10)
                .await
                .unwrap()
                .is_empty(),
            "{query:?} should answer nothing rather than fail"
        );
    }
}

#[tokio::test]
async fn a_title_filter_crosses_the_seam_as_a_predicate() {
    let (api, _tmp) = api().await;
    seed(&api).await;

    let filter = BookFilterDto {
        title: Some("station".into()),
        ..Default::default()
    };
    assert_eq!(api.count_books(Some(filter.clone())).await.unwrap(), 1);
    let rows = api
        .list_books(BookQueryDto {
            sort: Default::default(),
            filter: Some(filter),
            limit: 10,
            offset: 0,
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);

    let both = BookFilterDto {
        title: Some("station".into()),
        author: Some("mandel".into()),
        ..Default::default()
    };
    assert_eq!(api.count_books(Some(both)).await.unwrap(), 1);

    let none = BookFilterDto {
        title: Some("thermodynamics".into()),
        ..Default::default()
    };
    assert_eq!(api.count_books(Some(none)).await.unwrap(), 0);
}

#[tokio::test]
async fn a_closed_reading_crosses_as_a_moment_naming_its_read() {
    let (api, _tmp) = api().await;
    let book = seed(&api).await;

    assert_eq!(api.pending_moments(None).await.unwrap(), vec![]);

    api.update_progress(book, Some(333), Some(true))
        .await
        .unwrap();

    let typed = api.pending_moments(None).await.unwrap();
    let closed: Vec<_> = typed
        .iter()
        .filter(|m| m.kind == MomentKindDto::ReadingClosed)
        .collect();
    assert_eq!(closed.len(), 1, "one read ended: {typed:?}");
    assert_eq!(closed[0].book_id, Some(book));
    assert!(
        closed[0].reading_id.is_some(),
        "a card is minted per reading, so the moment has to name one"
    );

    match ok(api.dispatch(Request::PendingMoments { limit: None }).await) {
        Response::Moments(ms) => assert_eq!(ms, typed),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn acknowledging_a_moment_retires_it_and_repeating_it_changes_nothing() {
    let (api, _tmp) = api().await;
    let book = seed(&api).await;
    api.update_progress(book, Some(333), Some(true))
        .await
        .unwrap();

    let id = api.pending_moments(None).await.unwrap()[0].id.clone();
    ok(api
        .dispatch(Request::AcknowledgeMoment { id: id.clone() })
        .await);
    assert!(
        api.pending_moments(None)
            .await
            .unwrap()
            .iter()
            .all(|m| m.id != id)
    );

    api.acknowledge_moment(&id).await.unwrap();
    ok(api
        .dispatch(Request::AcknowledgeMoment { id: id.clone() })
        .await);
    assert!(
        api.pending_moments(None)
            .await
            .unwrap()
            .iter()
            .all(|m| m.id != id)
    );
}

#[tokio::test]
async fn an_invented_moment_id_is_refused() {
    let (api, _tmp) = api().await;
    let err = api
        .acknowledge_moment("something_i_made_up:1")
        .await
        .expect_err("a moment id is not a string a client composes");
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

#[test]
fn the_wire_states_no_number_of_moments() {
    let json = serde_json::to_string(&Response::Moments(vec![])).unwrap();
    assert_eq!(
        json, r#"{"shape":"moments","value":[]}"#,
        "a bare array, with no number beside it"
    );

    for name in ["count_moments", "pending_moment_count", "moment_count"] {
        let call = format!(r#"{{"method":"{name}","params":{{}}}}"#);
        assert!(
            serde_json::from_str::<Request>(&call).is_err(),
            "{name} must not exist: a number waiting is a badge"
        );
    }
}

#[tokio::test]
async fn a_limit_takes_the_newest_moments() {
    let (api, _tmp) = api().await;
    let a = seed(&api).await;
    let b = api
        .save_book(BookDto {
            title: Some("Middlemarch".into()),
            isbn_13: Some("9780141439549".into()),
            page_count: Some(880),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    api.update_progress(a, Some(333), Some(true)).await.unwrap();
    api.update_progress(b, Some(880), Some(true)).await.unwrap();

    let all = api.pending_moments(None).await.unwrap();
    assert!(all.len() >= 2);
    assert!(all.windows(2).all(|w| w[0].occurred_at >= w[1].occurred_at));

    match ok(api
        .dispatch(Request::PendingMoments { limit: Some(1) })
        .await)
    {
        Response::Moments(ms) => {
            assert_eq!(ms.len(), 1);
            assert_eq!(ms[0], all[0]);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_card_can_be_made_across_the_seam_and_the_second_is_not_new() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    assert!(
        api.create_flashcard(book_id, None, "middlemarch", Some("Ch 1"))
            .await
            .unwrap()
    );

    match ok(api
        .dispatch(Request::CreateFlashcard {
            book_id,
            highlight_id: None,
            word: "middlemarch".into(),
            context: Some("Ch 9".into()),
        })
        .await)
    {
        Response::Bool(created) => assert!(!created, "you already had this card"),
        other => panic!("{other:?}"),
    }

    let cards = api.list_flashcards_for_book(book_id).await.unwrap();
    assert_eq!(cards.len(), 1, "UNIQUE(book_id, word) deduped");
    assert_eq!(
        cards[0].book_id, book_id,
        "the card carries its book's id, not only its title"
    );
    assert_eq!(cards[0].highlight_id, None);
    assert_eq!(
        cards[0].context.as_deref(),
        Some("Ch 1"),
        "the second attempt must not rewrite the first card"
    );
}

#[tokio::test]
async fn a_card_pointing_at_nothing_is_refused_by_name() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    let err = api
        .create_flashcard(book_id, Some(4_242), "ghost", None)
        .await
        .expect_err("no such highlight");
    assert_eq!(err.code, ErrorCode::NotFound);

    let err = api
        .create_flashcard(9_999, None, "ghost", None)
        .await
        .expect_err("no such book");
    assert_eq!(err.code, ErrorCode::NotFound);

    match api
        .dispatch(Request::CreateFlashcard {
            book_id,
            highlight_id: None,
            word: "   ".into(),
            context: None,
        })
        .await
    {
        Err(e) => assert_eq!(e.code, ErrorCode::InvalidInput),
        Ok(other) => panic!("{other:?}"),
    }
    assert!(api.list_flashcards(true).await.unwrap().is_empty());
}

#[tokio::test]
async fn the_anki_export_crosses_the_seam_both_ways() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.create_flashcard(book_id, None, "middlemarch", Some("a <game> & more"))
        .await
        .unwrap();

    let typed = api.export_anki(Some(book_id), true).await.unwrap();
    assert_eq!(typed.count, 1);
    assert!(typed.text.contains("a &lt;game&gt; &amp; more"));
    assert!(typed.text.contains("bookmole::Station Eleven"));

    match ok(api
        .dispatch(Request::ExportAnki {
            book_id: Some(book_id),
            include_exported: true,
        })
        .await)
    {
        Response::AnkiExport(dto) => {
            assert_eq!(dto.text, typed.text, "the two paths are one call");
            assert_eq!(dto.count, 1);
        }
        other => panic!("{other:?}"),
    }

    match ok(api
        .dispatch(Request::SaveAnkiExport {
            book_id: None,
            include_exported: false,
        })
        .await)
    {
        Response::AnkiSaved(dto) => {
            assert_eq!(dto.count, 0);
            assert_eq!(dto.path, None);
        }
        other => panic!("{other:?}"),
    }
    let saved = api.save_anki_export(None, true).await.unwrap();
    let path = saved.path.expect("written");
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("#guid column:5")
    );

    let err = api.export_anki(Some(9_999), false).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);

    let req: Request = serde_json::from_str(r#"{"method":"export_anki","params":{}}"#).unwrap();
    assert_eq!(
        req,
        Request::ExportAnki {
            book_id: None,
            include_exported: false
        }
    );
}

#[tokio::test]
async fn the_citation_batch_answers_every_id_asked_in_order() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    let mut ids = Vec::new();
    for body in ["The first note.", "The second note."] {
        ids.push(
            api.create_note(NewNoteDto {
                book_id: Some(book_id),
                body: body.into(),
                ..Default::default()
            })
            .await
            .unwrap()
            .id,
        );
    }
    let asked = vec![ids[1], 9_999, ids[0], ids[1]];

    let typed = api.citations_for_notes(&asked).await.unwrap();
    assert_eq!(
        typed.iter().map(|r| r.note_id).collect::<Vec<_>>(),
        asked,
        "one row per id, duplicates included, in the order asked"
    );
    assert!(typed.iter().all(|r| r.highlight_ids.is_empty()));

    match ok(api
        .dispatch(Request::CitationsForNotes {
            note_ids: asked.clone(),
        })
        .await)
    {
        Response::NoteCitations(dispatched) => assert_eq!(dispatched, typed),
        other => panic!("{other:?}"),
    }

    assert!(api.citations_for_notes(&[]).await.unwrap().is_empty());
}

#[test]
fn the_citation_batch_puts_no_highlight_text_on_the_wire() {
    let json = serde_json::to_string(&Response::NoteCitations(vec![
        bookmole_api::NoteCitationsDto {
            note_id: 3,
            highlight_ids: vec![7, 9],
        },
    ]))
    .unwrap();
    assert_eq!(
        json,
        r#"{"shape":"note_citations","value":[{"note_id":3,"highlight_ids":[7,9]}]}"#
    );
    assert!(!json.contains("text"));
}

#[test]
fn a_payload_written_before_the_cards_wave_still_parses() {
    let single: Request =
        serde_json::from_str(r#"{"method":"citations_for","params":{"note_id":3}}"#).unwrap();
    assert_eq!(single, Request::CitationsFor { note_id: 3 });

    let cards: Request =
        serde_json::from_str(r#"{"method":"list_flashcards","params":{}}"#).unwrap();
    assert_eq!(
        cards,
        Request::ListFlashcards {
            include_exported: false
        }
    );

    let minimal: Request = serde_json::from_str(
        r#"{"method":"create_flashcard","params":{"book_id":4,"word":"mot"}}"#,
    )
    .unwrap();
    assert_eq!(
        minimal,
        Request::CreateFlashcard {
            book_id: 4,
            highlight_id: None,
            word: "mot".into(),
            context: None,
        }
    );
    let batch: Request =
        serde_json::from_str(r#"{"method":"citations_for_notes","params":{"note_ids":[3,4]}}"#)
            .unwrap();
    assert_eq!(
        batch,
        Request::CitationsForNotes {
            note_ids: vec![3, 4]
        }
    );
}

#[test]
fn the_new_reading_methods_parse_from_the_smallest_honest_payload() {
    let rows: Request =
        serde_json::from_str(r#"{"method":"list_reading_rows","params":{"limit":20}}"#).unwrap();
    assert_eq!(
        rows,
        Request::ListReadingRows {
            limit: 20,
            sort: Default::default(),
            offset: 0,
            filter: None,
        }
    );
    let count: Request =
        serde_json::from_str(r#"{"method":"count_readings","params":{}}"#).unwrap();
    assert_eq!(count, Request::CountReadings { filter: None });

    let old: Request =
        serde_json::from_str(r#"{"method":"list_readings","params":{"book_id":3}}"#).unwrap();
    assert_eq!(old, Request::ListReadings { book_id: 3 });
}

#[tokio::test]
async fn a_page_of_readings_carries_what_a_card_needs_in_one_call() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.update_progress(book_id, Some(333), Some(true))
        .await
        .unwrap();
    api.reread(book_id).await.unwrap();

    let query = ReadingQueryDto {
        limit: -1,
        filter: Some(ReadingFilterDto {
            book_id: Some(book_id),
            ..Default::default()
        }),
        ..Default::default()
    };
    let typed = api.list_reading_rows(query.clone()).await.unwrap();
    assert_eq!(typed.len(), 2, "a reread is a second row, not a flag");
    for row in &typed {
        assert_eq!(row.book.id, Some(book_id), "the book rides with the row");
        assert_eq!(row.of_reads, 2);
    }
    assert_eq!(
        typed.iter().map(|r| r.read_number).collect::<Vec<_>>(),
        vec![1, 2],
        "most recently finished first — and the open reread has no `finished_at`
         at all, so it lands last, which is where a read that has not ended
         belongs on a list of reads that did"
    );
    assert_eq!(typed[1].reading.finished_at, None);

    match ok(api
        .dispatch(Request::ListReadingRows {
            limit: -1,
            sort: Default::default(),
            offset: 0,
            filter: query.filter.clone(),
        })
        .await)
    {
        Response::ReadingRows(rows) => assert_eq!(rows, typed),
        other => panic!("{other:?}"),
    }

    assert_eq!(api.count_readings(query.filter.clone()).await.unwrap(), 2);
    match ok(api
        .dispatch(Request::CountReadings {
            filter: query.filter.clone(),
        })
        .await)
    {
        Response::Count(n) => assert_eq!(n, 2),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_readings_list_carries_no_passage_and_the_wall_does() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.update_progress(book_id, Some(12), None).await.unwrap();

    let plain = api.list_readings(book_id).await.unwrap();
    let json = serde_json::to_value(&plain[0]).unwrap();
    assert!(
        json.get("passage").is_none(),
        "a reading is not a card: {json}"
    );

    let rows = api
        .list_reading_rows(ReadingQueryDto {
            limit: -1,
            ..Default::default()
        })
        .await
        .unwrap();
    let json = serde_json::to_value(&rows[0]).unwrap();
    assert!(json.get("passage").is_some(), "the wall's row carries it");
    for absent in ["rating", "notes", "reflection", "review"] {
        assert!(
            json.get(absent).is_none(),
            "{absent} would make this a CardDto, a layout this crate does not serve: {json}"
        );
    }
}

#[tokio::test]
async fn a_backwards_year_is_refused_by_both_doors() {
    let (api, _tmp) = api().await;
    let filter = ReadingFilterDto {
        finished_in: Some(DayRangeDto {
            from: "2025-12-31".into(),
            to: "2025-01-01".into(),
        }),
        ..Default::default()
    };

    let err = api
        .list_reading_rows(ReadingQueryDto {
            limit: 20,
            filter: Some(filter.clone()),
            ..Default::default()
        })
        .await
        .expect_err("a backwards range is a refusal");
    assert_eq!(err.code, ErrorCode::InvalidInput);

    let err = api
        .count_readings(Some(filter.clone()))
        .await
        .expect_err("and the count refuses it identically");
    assert_eq!(err.code, ErrorCode::InvalidInput);

    match api
        .dispatch(Request::CountReadings {
            filter: Some(filter),
        })
        .await
    {
        Err(e) => assert_eq!(e.code, ErrorCode::InvalidInput),
        Ok(other) => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_year_picker_gets_the_years_and_the_open_bucket() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.update_progress(book_id, Some(333), Some(true))
        .await
        .unwrap();
    api.reread(book_id).await.unwrap();

    let typed = api.reading_years(None).await.unwrap();
    assert_eq!(typed.years.len(), 1, "one read has ended, so one year");
    assert!(typed.open, "the reread is open and belongs to no year");

    match ok(api.dispatch(Request::ReadingYears { filter: None }).await) {
        Response::ReadingYears(y) => assert_eq!(y, typed),
        other => panic!("{other:?}"),
    }

    for year in &typed.years {
        let scoped = ReadingFilterDto {
            finished_in: Some(DayRangeDto {
                from: format!("{year}-01-01"),
                to: format!("{year}-12-31"),
            }),
            ..Default::default()
        };
        assert!(
            api.count_readings(Some(scoped)).await.unwrap() > 0,
            "{year} was offered and the wall has nothing in it"
        );
    }
}

#[test]
fn the_year_request_parses_from_the_smallest_honest_payload() {
    let years: Request = serde_json::from_str(r#"{"method":"reading_years","params":{}}"#).unwrap();
    assert_eq!(years, Request::ReadingYears { filter: None });

    let count: Request =
        serde_json::from_str(r#"{"method":"count_readings","params":{}}"#).unwrap();
    assert_eq!(count, Request::CountReadings { filter: None });
    assert_eq!(bookmole_api::API_VERSION, 2);
}

#[tokio::test]
async fn a_backwards_year_is_refused_by_the_year_list_as_well() {
    let (api, _tmp) = api().await;
    let filter = ReadingFilterDto {
        finished_in: Some(DayRangeDto {
            from: "2025-12-31".into(),
            to: "2025-01-01".into(),
        }),
        ..Default::default()
    };
    let err = api
        .reading_years(Some(filter.clone()))
        .await
        .expect_err("a backwards range is a refusal");
    assert_eq!(err.code, ErrorCode::InvalidInput);

    match api
        .dispatch(Request::ReadingYears {
            filter: Some(filter),
        })
        .await
    {
        Err(e) => assert_eq!(e.code, ErrorCode::InvalidInput),
        Ok(other) => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_year_list_puts_no_count_on_the_wire() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.update_progress(book_id, Some(333), Some(true))
        .await
        .unwrap();

    let json = serde_json::to_string(&api.reading_years(None).await.unwrap()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    // Keys are sorted: wire key order varies when any workspace crate enables serde_json's
    // `preserve_order`.
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["open", "years"],
        "two fields, and neither counts"
    );
    assert!(
        value["years"]
            .as_array()
            .unwrap()
            .iter()
            .all(|y| y.is_i64()),
        "a year is a number; a year carrying a number is a scoreboard"
    );
}

/// The DTO's `Default` and the engine's must be the same query (`limit: -1`, not 0).
#[test]
fn the_two_reading_query_defaults_are_one_query() {
    let dto = ReadingQueryDto::default();
    let engine: bookmole::ReadingQuery = dto.clone().try_into().unwrap();
    assert_eq!(engine, bookmole::ReadingQuery::default());
    assert!(dto.limit < 0, "the whole-library read has one spelling");
}

/// A plausible KOReader install, hand-rolled because the engine's `#[cfg(test)]` helpers are not
/// visible here.
fn fake_reader(root: &std::path::Path) {
    let dir = root.join("koreader");
    std::fs::create_dir_all(dir.join("frontend")).unwrap();
    std::fs::create_dir_all(dir.join("plugins")).unwrap();
    std::fs::write(dir.join("reader.lua"), "-- entry point\n").unwrap();
}

#[tokio::test]
async fn the_plugin_seam_round_trips_through_dispatch() {
    let (api, _tmp) = api().await;
    let reader = tempfile::tempdir().unwrap();
    fake_reader(reader.path());
    let mount = reader.path().display().to_string();

    let before = match ok(api
        .dispatch(Request::PluginStatus {
            mount: mount.clone(),
        })
        .await)
    {
        Response::PluginStatus(s) => s,
        other => panic!("{other:?}"),
    };
    assert!(!before.installed);
    assert!(!before.paired);
    assert!(before.plugin_dir.ends_with("bookmole.koplugin"));

    let report = match ok(api
        .dispatch(Request::InstallPlugin {
            mount: mount.clone(),
        })
        .await)
    {
        Response::PluginInstalled(r) => r,
        other => panic!("{other:?}"),
    };
    assert_eq!(report.version, before.our_version);
    assert_eq!(report.upgraded_from, None);

    let after = match ok(api
        .dispatch(Request::PluginStatus {
            mount: mount.clone(),
        })
        .await)
    {
        Response::PluginStatus(s) => s,
        other => panic!("{other:?}"),
    };
    assert!(after.installed && after.paired);
    assert_eq!(after.device_id.as_deref(), Some(report.device_id.as_str()));

    let paired = match ok(api.dispatch(Request::PairedDevices).await) {
        Response::PairedDevices(d) => d,
        other => panic!("{other:?}"),
    };
    assert_eq!(paired.len(), 1);
    assert_eq!(paired[0].device_id, report.device_id);

    match ok(api.dispatch(Request::UninstallPlugin { mount }).await) {
        Response::PluginUninstalled(r) => {
            assert_eq!(r.forgot_device.as_deref(), Some(report.device_id.as_str()))
        }
        other => panic!("{other:?}"),
    }
    assert!(api.paired_devices().await.unwrap().is_empty());
}

#[tokio::test]
async fn no_reply_ever_carries_the_pairing_token() {
    let (api, _tmp) = api().await;
    let reader = tempfile::tempdir().unwrap();
    fake_reader(reader.path());
    let mount = reader.path().display().to_string();

    api.dispatch(Request::InstallPlugin {
        mount: mount.clone(),
    })
    .await
    .unwrap();

    // Read the token off the device, not a constant, so this fails if the value ever travels.
    let pairing = std::fs::read_to_string(
        reader
            .path()
            .join("koreader/plugins/bookmole.koplugin/pairing.lua"),
    )
    .unwrap();
    let token = pairing
        .lines()
        .find_map(|l| l.trim().strip_prefix("token     = \""))
        .and_then(|l| l.strip_suffix("\","))
        .expect("the installer wrote a token");
    assert_eq!(token.len(), 64);

    for request in [Request::PairedDevices, Request::PluginStatus { mount }] {
        let reply = serde_json::to_string(&ok(api.dispatch(request).await)).unwrap();
        assert!(!reply.contains(token), "a reply carried the token: {reply}");
    }
}

#[tokio::test]
async fn refusing_an_unrecognised_volume_is_its_own_error_code() {
    let (api, _tmp) = api().await;
    let plain = tempfile::tempdir().unwrap();

    let err = api
        .dispatch(Request::InstallPlugin {
            mount: plain.path().display().to_string(),
        })
        .await
        .expect_err("an ordinary directory is not a reader");
    assert_eq!(err.code, ErrorCode::PluginRefused);
    assert_eq!(std::fs::read_dir(plain.path()).unwrap().count(), 0);
}

/// `last_seen_at` and `last_synced_at` are asserted apart: a look must not read as a sync.
#[tokio::test]
async fn the_devices_seam_round_trips_through_dispatch() {
    let (api, _tmp) = api().await;
    let reader = tempfile::tempdir().unwrap();
    fake_reader(reader.path());
    let mount = reader.path().display().to_string();

    let report = match ok(api
        .dispatch(Request::InstallPlugin {
            mount: mount.clone(),
        })
        .await)
    {
        Response::PluginInstalled(r) => r,
        other => panic!("{other:?}"),
    };

    ok(api
        .dispatch(Request::PluginStatus {
            mount: mount.clone(),
        })
        .await);
    let seen = api.paired_devices().await.unwrap();
    assert!(seen[0].last_seen_at.is_some());
    assert_eq!(
        seen[0].last_synced_at, None,
        "looking at a reader is not syncing with it"
    );
    assert_eq!(seen[0].last_mount_path.as_deref(), Some(mount.as_str()));

    // An empty reader: `found` and `synced` are zero yet the device is still identified.
    let sync = match ok(api
        .dispatch(Request::SyncMount {
            mount: mount.clone(),
        })
        .await)
    {
        Response::MountSync(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!(sync.device_id.as_deref(), Some(report.device_id.as_str()));
    assert_eq!((sync.found, sync.synced), (0, 0));
    assert!(sync.reports.is_empty());
    assert!(
        api.paired_devices().await.unwrap()[0]
            .last_synced_at
            .is_some()
    );

    let after = &api.paired_devices().await.unwrap()[0];
    assert_eq!(after.last_wireless_at, None, "a cable is not the air");
    assert_eq!(after.last_lan_addr, None);

    match ok(api
        .dispatch(Request::RenameDevice {
            device_id: report.device_id.clone(),
            label: "the bedside Kobo".into(),
        })
        .await)
    {
        Response::Bool(done) => assert!(done),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        api.paired_devices().await.unwrap()[0].label.as_deref(),
        Some("the bedside Kobo")
    );
    ok(api
        .dispatch(Request::RenameDevice {
            device_id: report.device_id.clone(),
            label: "  ".into(),
        })
        .await);
    assert_eq!(api.paired_devices().await.unwrap()[0].label, None);

    match ok(api
        .dispatch(Request::ForgetDevice {
            device_id: report.device_id.clone(),
        })
        .await)
    {
        Response::Bool(done) => assert!(done),
        other => panic!("{other:?}"),
    }
    assert!(api.paired_devices().await.unwrap().is_empty());
    let still = api.plugin_status(reader.path()).await.unwrap();
    assert!(
        still.installed && !still.paired,
        "forgetting must not reach the reader — the plugin is still on it"
    );

    match ok(api
        .dispatch(Request::ForgetDevice {
            device_id: report.device_id,
        })
        .await)
    {
        Response::Bool(done) => assert!(!done),
        other => panic!("{other:?}"),
    }
}

/// A non-reader mount must sync and stamp nothing; a failed plugin inspection must not fail the
/// sync.
#[tokio::test]
async fn syncing_a_tree_that_is_not_a_reader_is_ordinary() {
    let (api, _tmp) = api().await;
    let tree = tempfile::tempdir().unwrap();
    let sync = match ok(api
        .dispatch(Request::SyncMount {
            mount: tree.path().display().to_string(),
        })
        .await)
    {
        Response::MountSync(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!(sync.device_id, None);
    assert_eq!((sync.found, sync.synced), (0, 0));
    assert!(api.paired_devices().await.unwrap().is_empty());
}

#[test]
fn a_payload_written_before_the_devices_page_still_parses() {
    let before: Request =
        serde_json::from_str(r#"{"method":"sync_device","params":{"paths":["/mnt/k"]}}"#).unwrap();
    assert!(matches!(before, Request::SyncDevice { .. }));
    let before: Request = serde_json::from_str(r#"{"method":"paired_devices"}"#).unwrap();
    assert!(matches!(before, Request::PairedDevices));
    assert_eq!(
        bookmole_api::API_VERSION,
        2,
        "these added methods and reshaped none"
    );
}

/// A non-code is refused as `wireless_refused` before any socket opens, without echoing what was
/// typed.
#[tokio::test]
async fn pairing_over_wifi_is_additive_and_refuses_what_is_not_a_code() {
    let (api, _tmp) = api().await;

    let bare: Request =
        serde_json::from_str(r#"{"method":"pair_with_reader","params":{"code":"ABCD"}}"#).unwrap();
    assert!(matches!(bare, Request::PairWithReader { host: None, .. }));
    let with_host: Request = serde_json::from_str(
        r#"{"method":"pair_with_reader","params":{"code":"ABCD","host":"192.0.2.42"}}"#,
    )
    .unwrap();
    assert!(matches!(
        with_host,
        Request::PairWithReader { host: Some(_), .. }
    ));

    let err = api
        .dispatch(Request::PairWithReader {
            code: "hunter2-is-not-a-code".into(),
            host: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::WirelessRefused);
    assert!(
        !err.message.contains("hunter2"),
        "a refusal must not echo the code: {}",
        err.message
    );
    assert!(api.paired_devices().await.unwrap().is_empty());

    // The key must be present even with no address: bindings are keyed by name.
    let status = serde_json::to_value(api.listener_status().await.unwrap()).unwrap();
    assert!(status.get("lan_address").is_some(), "{status}");

    assert_eq!(
        bookmole_api::API_VERSION,
        2,
        "pairing over wifi added a method and reshaped none"
    );
}

#[tokio::test]
async fn resurface_note_takes_the_callers_day_and_answers_null_when_nothing_qualifies() {
    let (api, _tmp) = api().await;
    let created = api
        .create_note(NewNoteDto {
            body: "An older thought.".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let now = time::OffsetDateTime::now_utc().date();
    let day = |d: time::Date| format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day());

    match ok(api
        .dispatch(Request::ResurfaceNote { today: day(now) })
        .await)
    {
        Response::Resurfaced(None) => {}
        other => panic!("a note kept today rests, got {other:?}"),
    }

    let later = day(now + time::Duration::days(30));
    match ok(api.dispatch(Request::ResurfaceNote { today: later }).await) {
        Response::Resurfaced(Some(r)) => {
            assert_eq!(r.note.id, created.id);
            assert_eq!(r.reason, bookmole_api::ResurfaceReasonDto::Notebook);
        }
        other => panic!("expected the note back, got {other:?}"),
    }

    let bad = api.resurface_note("18/10/2026").await.unwrap_err();
    assert_eq!(bad.code, ErrorCode::InvalidInput);
}

#[tokio::test]
async fn resurface_passage_crosses_the_seam_like_the_note() {
    let (api, _tmp) = api().await;
    let now = time::OffsetDateTime::now_utc().date();
    let day = format!(
        "{:04}-{:02}-{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    );
    match ok(api
        .dispatch(Request::ResurfacePassage { today: day.clone() })
        .await)
    {
        Response::ResurfacedPassage(None) => {}
        other => panic!("an empty library offers no passage, got {other:?}"),
    }
    assert_eq!(api.resurface_passage(&day).await.unwrap(), None);
    let parsed: Request =
        serde_json::from_str(r#"{"method":"resurface_passage","params":{"today":"2026-11-10"}}"#)
            .unwrap();
    assert!(matches!(parsed, Request::ResurfacePassage { .. }));
    let bad = api.resurface_passage("10/11/2026").await.unwrap_err();
    assert_eq!(bad.code, ErrorCode::InvalidInput);
}

#[tokio::test]
async fn link_contexts_cross_the_seam_with_their_paragraph() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    let target = api
        .create_note(NewNoteDto {
            book_id: Some(book_id),
            title: Some("Symphony".into()),
            body: "Out to [[Nowhere]].".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let source = api
        .create_note(NewNoteDto {
            book_id: Some(book_id),
            title: Some("Travelling".into()),
            body: "Head.\n\nThe [[Symphony]] plays on.".into(),
            ..Default::default()
        })
        .await
        .unwrap();

    let typed = api.link_contexts(target.id).await.unwrap();
    assert_eq!(typed.len(), 2);
    assert_eq!(typed[0].direction, LinkDirectionDto::Incoming);
    assert_eq!(typed[0].note.as_ref().map(|n| n.id), Some(source.id));
    assert_eq!(typed[0].book.as_ref().map(|b| b.id), Some(book_id));
    assert_eq!(
        typed[0].paragraph.as_deref(),
        Some("The [[Symphony]] plays on.")
    );
    assert_eq!(typed[0].link_range, Some((4, 16)));
    assert!(!typed[0].truncated);
    assert_eq!(typed[1].direction, LinkDirectionDto::Outgoing);
    assert_eq!(typed[1].target_title, "Nowhere");
    assert!(typed[1].note.is_none() && typed[1].book.is_none());
    assert!(typed[1].paragraph.is_none() && typed[1].link_range.is_none());

    match ok(api
        .dispatch(Request::LinkContexts { note_id: target.id })
        .await)
    {
        Response::LinkContexts(rows) => assert_eq!(rows, typed),
        other => panic!("expected link contexts, got {other:?}"),
    }

    let json = serde_json::to_value(&typed[0]).unwrap();
    assert_eq!(json["direction"], "incoming");
    assert_eq!(json["link_range"], serde_json::json!([4, 16]));
}

#[tokio::test]
#[expect(clippy::too_many_lines)]
async fn the_notes_boards_round_trip_through_dispatch() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    let hub = api
        .create_note(NewNoteDto {
            book_id: Some(book_id),
            body: "# Symphony\n\nThe travelling symphony plays.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let spoke = api
        .create_note(NewNoteDto {
            title: Some("Kirsten".into()),
            body: "She joins [[Symphony]] and [[Unwritten]]. Later, kirsten again.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let mentioner = api
        .create_note(NewNoteDto {
            title: Some("Arthur".into()),
            body: "Arthur never met Kirsten.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(hub.title, "Symphony", "the heading names the note");

    let typed = api
        .list_note_rows(None, None, None, None, None, 10)
        .await
        .unwrap();
    match ok(api
        .dispatch(Request::ListNoteRows {
            kinds: None,
            book_id: None,
            reading_id: None,
            ids: None,
            until: None,
            limit: 10,
        })
        .await)
    {
        Response::NoteRows(rows) => assert_eq!(rows, typed),
        other => panic!("{other:?}"),
    }
    let hub_row = typed.iter().find(|r| r.note.id == hub.id).unwrap();
    assert_eq!(hub_row.heading.as_deref(), Some("Symphony"));
    assert_eq!(hub_row.excerpt, "The travelling symphony plays.");
    assert_eq!(hub_row.links_in, 1);
    assert!(hub_row.thread.is_some(), "linked, so in a thread");
    assert!(hub_row.book.is_some());
    let refused = api
        .dispatch(Request::ListNoteRows {
            kinds: Some(vec![NoteKindDto::Note]),
            book_id: Some(book_id),
            reading_id: Some(1),
            ids: None,
            until: None,
            limit: 10,
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);

    match ok(api.dispatch(Request::NotePeriods).await) {
        Response::NotePeriods(p) => assert_eq!(p.len(), 1),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ThreadsFor {
            note_ids: vec![spoke.id],
            since: Some(0),
        })
        .await)
    {
        Response::Threads(t) => {
            assert_eq!(t.len(), 1);
            assert_eq!(t[0].notes, 2);
            assert_eq!(t[0].joined, 2);
        }
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::LinksFromNotes {
            note_ids: vec![spoke.id],
        })
        .await)
    {
        Response::NoteLinks(l) => {
            let titles: Vec<_> = l.iter().map(|l| l.target_title.as_str()).collect();
            assert_eq!(titles, vec!["Symphony", "Unwritten"]);
        }
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::NoteGraph {
            scope: GraphScopeDto::Around {
                note_id: spoke.id,
                steps: 1,
            },
        })
        .await)
    {
        Response::NoteGraph(g) => {
            assert!(
                g.nodes
                    .iter()
                    .any(|n| n.key == "tunwritten" && n.row.is_none())
            );
            assert!(g.edges.iter().any(|e| e.kind == GraphEdgeKindDto::Mention));
        }
        other => panic!("{other:?}"),
    }
    let json = serde_json::to_value(GraphScopeDto::Recent { since: 5 }).unwrap();
    assert_eq!(json, serde_json::json!({"scope": "recent", "since": 5}));
    match ok(api
        .dispatch(Request::ContinueNote {
            note_id: hub.id,
            body: "A second sitting.".into(),
            page: Some(12),
        })
        .await)
    {
        Response::NoteParts(p) => assert_eq!(p.len(), 2),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::UpdateNotePart {
            note_id: hub.id,
            n: 2,
            body: "Revised.".into(),
        })
        .await)
    {
        Response::NoteParts(p) => assert_eq!(p[1].body, "Revised."),
        other => panic!("{other:?}"),
    }
    match ok(api.dispatch(Request::NoteParts { note_id: hub.id }).await) {
        Response::NoteParts(p) => assert_eq!((p[1].n, p[1].page), (2, Some(12))),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::UnlinkedMentions {
            note_id: spoke.id,
            limit: 5,
        })
        .await)
    {
        Response::SearchHits(h) => {
            assert!(matches!(&h[..], [SearchHitDto::Note { note, .. }] if note.id == mentioner.id))
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        ok(api
            .dispatch(Request::LinkMention {
                from_note_id: mentioner.id,
                to_note_id: spoke.id,
            })
            .await),
        Response::Unit
    ));
    match ok(api
        .dispatch(Request::CreateHighlight {
            book_id,
            text: "Survival is insufficient.".into(),
            page: Some(58),
            chapter: None,
        })
        .await)
    {
        Response::Highlight(Some(h)) => assert_eq!((h.source.as_str(), h.page), ("user", Some(58))),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::PreviewLinks {
            title: Some("Unwritten".into()),
            body: "see [[Kirsten]]".into(),
        })
        .await)
    {
        Response::LinkPreview(p) => {
            assert_eq!(p.outgoing[0].note.as_ref().map(|n| n.id), Some(spoke.id));
            assert_eq!(p.incoming.len(), 1);
            assert_eq!(p.incoming[0].direction, LinkDirectionDto::Incoming);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
#[expect(clippy::too_many_lines)]
async fn history_library_and_the_notes_gaps_round_trip_through_dispatch() {
    use bookmole_api::{
        HistoryCompareDto, HistoryGroupDto, HistoryMetricDto, HistorySplitDto, NoteCursorDto,
        ReadMarkDto,
    };
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;
    api.update_progress(book_id, Some(10), None).await.unwrap();
    let passage = api
        .create_highlight(book_id, "a passage", Some(10), None)
        .await
        .unwrap();
    api.update_progress(book_id, None, Some(true))
        .await
        .unwrap();
    let reading = api.list_readings(book_id).await.unwrap()[0].id;
    let review = api.open_review(book_id, Some(reading)).await.unwrap();
    api.set_rating(review.id, 4.0).await.unwrap();

    ok(api
        .dispatch(Request::SetSetting {
            key: "history.overview.cards".into(),
            value: r#"["pages"]"#.into(),
        })
        .await);
    match ok(api
        .dispatch(Request::GetSetting {
            key: "history.overview.cards".into(),
        })
        .await)
    {
        Response::Setting(v) => assert_eq!(v.as_deref(), Some(r#"["pages"]"#)),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ListSettings {
            prefix: "history.".into(),
        })
        .await)
    {
        Response::Settings(s) => assert_eq!(s.len(), 1),
        other => panic!("{other:?}"),
    }
    let refused = api
        .dispatch(Request::SetSetting {
            key: "Bad Key".into(),
            value: "1".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);

    let typed = api.history_timeline(None, None).await.unwrap();
    match ok(api
        .dispatch(Request::HistoryTimeline {
            since: None,
            until: None,
        })
        .await)
    {
        Response::HistoryTimeline(t) => {
            assert_eq!(t.len(), 1);
            assert_eq!(t[0].read.reading.id, typed[0].read.reading.id);
            assert_eq!(t[0].book.title.as_deref(), Some("Station Eleven"));
            assert_eq!(t[0].read.passages, 1);
            assert_eq!(t[0].read.rating.as_ref().map(|r| r.value), Some(4.0));
        }
        other => panic!("{other:?}"),
    }
    let inverted = api
        .dispatch(Request::HistoryTimeline {
            since: Some(10),
            until: Some(5),
        })
        .await
        .unwrap_err();
    assert_eq!(inverted.code, ErrorCode::InvalidInput);
    match ok(api
        .dispatch(Request::HistoryFigures {
            since: None,
            until: None,
        })
        .await)
    {
        Response::HistoryFigures(f) => {
            assert_eq!(f.books_finished, 1);
            assert_eq!(f.passages, 1);
            assert_eq!(f.reviews, 1);
            let json = serde_json::to_value(&f).unwrap();
            assert!(
                json.get("current_run").is_none() && json.get("streak").is_none(),
                "no current run crosses the wire"
            );
        }
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::HistoryRecords {
            since: None,
            until: None,
        })
        .await)
    {
        Response::HistoryRecords(r) => {
            assert_eq!(r.best_rated.unwrap().read.read.reading.id, reading);
            assert_eq!(r.longest.unwrap().pages, Some(333));
        }
        other => panic!("{other:?}"),
    }
    let series = |export: bool| {
        let (metric, group, split, compare) = (
            HistoryMetricDto::Passages,
            HistoryGroupDto::Week,
            HistorySplitDto::Book,
            HistoryCompareDto::LastYear,
        );
        if export {
            Request::ExportHistorySeries {
                metric,
                group,
                split,
                compare,
                since: None,
                until: None,
            }
        } else {
            Request::HistorySeries {
                metric,
                group,
                split,
                compare,
                since: None,
                until: None,
            }
        }
    };
    match ok(api.dispatch(series(false)).await) {
        Response::HistorySeries(s) => {
            assert_eq!(s.total, 1);
            assert_eq!(s.compare_total, Some(0));
            let bucket = &s.buckets[s.busiest.unwrap() as usize];
            assert_eq!(bucket.parts[0].label, "Station Eleven");
        }
        other => panic!("{other:?}"),
    }
    match ok(api.dispatch(series(true)).await) {
        Response::SavedCsv(c) => {
            assert!(std::path::Path::new(&c.path).exists());
            assert!(c.rows >= 2);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        serde_json::to_value(HistorySplitDto::Unsplit).unwrap(),
        "none",
        "the wire word is `none`"
    );
    match ok(api
        .dispatch(Request::HistoryKept {
            since: None,
            until: None,
            limit: 5,
        })
        .await)
    {
        Response::HistoryKept(k) => {
            assert_eq!((k[0].book.id, k[0].passages, k[0].notes), (book_id, 1, 1));
        }
        other => panic!("{other:?}"),
    }

    match ok(api.dispatch(Request::LibraryTable).await) {
        Response::LibraryTable(t) => {
            assert_eq!(t.counts.all, 1);
            assert_eq!(t.counts.read, 1);
            assert_eq!(t.total_pages, 333);
            assert_eq!(t.rows[0].passages, 1);
            assert!(t.rows[0].latest.is_some());
        }
        other => panic!("{other:?}"),
    }

    match ok(api.dispatch(Request::GetHighlight { id: passage.id }).await) {
        Response::Highlight(h) => assert_eq!(h.unwrap().id, passage.id),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::BookFaces {
            book_ids: vec![999, book_id],
        })
        .await)
    {
        Response::BookFaces(f) => {
            assert_eq!(f.iter().map(|f| f.id).collect::<Vec<_>>(), vec![book_id])
        }
        other => panic!("{other:?}"),
    }
    match ok(api.dispatch(Request::NoteBooks).await) {
        Response::NoteBooks(b) => assert!(b.is_empty(), "nothing links yet"),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ReadMarks {
            reading_id: reading,
            limit: -1,
        })
        .await)
    {
        Response::ReadMarks(m) => {
            assert_eq!(m.len(), 2);
            let note = m.iter().find_map(|m| match m {
                ReadMarkDto::Note { row, .. } => Some(row),
                ReadMarkDto::Passage { .. } => None,
            });
            assert_eq!(
                note.unwrap().read_number,
                Some(1),
                "the first read's review"
            );
        }
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ListNoteRowsAfter {
            kinds: None,
            book_id: None,
            reading_id: None,
            ids: None,
            until: None,
            after: Some(NoteCursorDto {
                at: i64::MAX,
                id: i64::MAX,
            }),
            limit: 10,
        })
        .await)
    {
        Response::NoteRows(r) => assert_eq!(r.len(), 1),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_cleanup_requests_round_trip_through_dispatch() {
    let (api, _tmp) = api().await;
    let book_id = seed(&api).await;

    let note = api
        .create_note(NewNoteDto {
            book_id: Some(book_id),
            title: Some("A log".into()),
            body: "The harbour at dusk.".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    api.continue_note(note.id, "Nothing about water.", None)
        .await
        .unwrap();
    api.continue_note(note.id, "A lighthouse, then harbours.", None)
        .await
        .unwrap();
    let typed = api.matched_parts(note.id, "harbour").await.unwrap();
    assert_eq!(typed, vec![1, 3]);
    let wire: Request = serde_json::from_str(&format!(
        r#"{{"method":"matched_parts","params":{{"note_id":{},"query":"harbour"}}}}"#,
        note.id
    ))
    .unwrap();
    match ok(api.dispatch(wire).await) {
        Response::Parts(p) => assert_eq!(p, typed),
        other => panic!("{other:?}"),
    }
    let missing = api
        .dispatch(Request::MatchedParts {
            note_id: note.id + 100,
            query: "harbour".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);

    let utc = api.note_periods().await.unwrap();
    let wire: Request =
        serde_json::from_str(r#"{"method":"note_periods_in","params":{"utc_offset_minutes":0}}"#)
            .unwrap();
    match ok(api.dispatch(wire).await) {
        Response::NotePeriods(p) => assert_eq!(p, utc),
        other => panic!("{other:?}"),
    }
    let refused = api
        .dispatch(Request::NotePeriodsIn {
            utc_offset_minutes: 2000,
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);

    api.update_progress(book_id, Some(10), None).await.unwrap();
    let typed = api.history_reading_now().await.unwrap();
    assert_eq!(typed.len(), 1);
    assert_eq!(typed[0].book.title.as_deref(), Some("Station Eleven"));
    let wire: Request = serde_json::from_str(r#"{"method":"history_reading_now"}"#).unwrap();
    match ok(api.dispatch(wire).await) {
        Response::HistoryTimeline(t) => {
            assert_eq!(t, typed);
            assert_eq!(t[0].read.reading.status, ReadingStateDto::Reading);
        }
        other => panic!("{other:?}"),
    }
    api.update_progress(book_id, None, Some(true))
        .await
        .unwrap();
    assert!(
        api.history_reading_now().await.unwrap().is_empty(),
        "a finished read is not reading now"
    );
}

async fn save(api: &Api, title: &str, isbn: &str) -> i64 {
    api.save_book(BookDto {
        title: Some(title.into()),
        authors: vec!["An Author".into()],
        isbn_13: Some(isbn.into()),
        ..Default::default()
    })
    .await
    .expect("save")
    .id
    .expect("id")
}

fn copy_of(response: Response) -> bookmole_api::CopyDto {
    match response {
        Response::Copy(c) => c,
        other => panic!("{other:?}"),
    }
}

fn list_of(response: Response) -> bookmole_api::ListDto {
    match response {
        Response::List(l) => l,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
#[expect(clippy::too_many_lines)]
async fn a_list_is_made_filled_ordered_and_deleted_by_request() {
    let (api, _tmp) = api().await;
    let a = save(&api, "Alpha", "9780000000002").await;
    let b = save(&api, "Bravo", "9780000000019").await;
    let c = save(&api, "Charlie", "9780000000026").await;
    have(&api, a).await;

    let list = list_of(ok(api
        .dispatch(Request::CreateList {
            name: "To read".into(),
            description: Some("soon".into()),
        })
        .await));
    assert_eq!((list.book_count, list.not_in_library), (0, 0));
    assert!(!list.timeline_ready);
    for book_id in [a, b, c] {
        match ok(api
            .dispatch(Request::AddToList {
                list_id: list.id,
                book_id,
                note: None,
            })
            .await)
        {
            Response::ListEntry(e) => assert_eq!(e.book.id, book_id),
            other => panic!("{other:?}"),
        }
    }
    let got = list_of(ok(api.dispatch(Request::GetList { id: list.id }).await));
    assert_eq!((got.book_count, got.not_in_library), (3, 2));

    ok(api
        .dispatch(Request::MoveInList {
            list_id: list.id,
            book_id: c,
            to: 0,
        })
        .await);
    ok(api
        .dispatch(Request::SetListNote {
            list_id: list.id,
            book_id: c,
            note: Some("start here".into()),
        })
        .await);
    let entries = |show, sort| {
        let api = api.clone();
        async move {
            match ok(api
                .dispatch(Request::ListEntries {
                    list_id: list.id,
                    show,
                    sort,
                })
                .await)
            {
                Response::ListEntries(v) => v,
                other => panic!("{other:?}"),
            }
        }
    };
    let yours = entries(EntryShowDto::All, EntrySortDto::Yours).await;
    assert_eq!(
        yours.iter().map(|e| e.book.id).collect::<Vec<_>>(),
        [c, a, b]
    );
    assert_eq!(yours[0].note.as_deref(), Some("start here"));
    let by_title = entries(EntryShowDto::All, EntrySortDto::Title).await;
    assert_eq!(
        by_title.iter().map(|e| e.book.id).collect::<Vec<_>>(),
        [a, b, c]
    );
    assert_eq!(
        entries(EntryShowDto::All, EntrySortDto::Yours)
            .await
            .iter()
            .map(|e| e.position)
            .collect::<Vec<_>>(),
        [0, 1, 2],
        "sorting by title did not renumber the list"
    );
    let outside = entries(EntryShowDto::NotInLibrary, EntrySortDto::Yours).await;
    assert_eq!(outside.len(), 2);
    assert!(outside.iter().all(|e| !e.in_library));

    match ok(api.dispatch(Request::ListsForBook { book_id: c }).await) {
        Response::ListMemberships(m) => {
            assert_eq!((m[0].position, m[0].list_size), (1, 3));
        }
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ListTimeline { list_id: list.id })
        .await)
    {
        Response::ListTimeline(t) => assert!(!t.ready && t.dated.is_empty()),
        other => panic!("{other:?}"),
    }
    match ok(api
        .dispatch(Request::ListLists {
            sort: ListSortDto::Name,
            preview: 2,
        })
        .await)
    {
        Response::ListSummaries(v) => {
            assert_eq!(v.len(), 1);
            assert_eq!((v[0].book_count, v[0].preview.len()), (3, 2));
        }
        other => panic!("{other:?}"),
    }

    let renamed = list_of(ok(api
        .dispatch(Request::UpdateList {
            id: list.id,
            name: "Next up".into(),
            description: None,
        })
        .await));
    assert_eq!(renamed.name, "Next up");
    assert_eq!(renamed.description, None);

    ok(api
        .dispatch(Request::RemoveFromList {
            list_id: list.id,
            book_id: a,
        })
        .await);
    assert_eq!(
        list_of(ok(api.dispatch(Request::GetList { id: list.id }).await)).book_count,
        2
    );

    let blank = api
        .dispatch(Request::CreateList {
            name: "   ".into(),
            description: None,
        })
        .await
        .unwrap_err();
    assert_eq!(blank.code, ErrorCode::InvalidInput);
    let missing = api
        .dispatch(Request::GetList { id: 999 })
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
    let no_book = api
        .dispatch(Request::AddToList {
            list_id: list.id,
            book_id: 999,
            note: None,
        })
        .await
        .unwrap_err();
    assert_eq!(no_book.code, ErrorCode::NotFound);

    ok(api.dispatch(Request::DeleteList { id: list.id }).await);
    assert_eq!(
        api.dispatch(Request::GetList { id: list.id })
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn a_shelf_is_kept_or_dismissed_by_request() {
    let (api, tmp) = api().await;
    let id = seed(&api).await;
    let csv = tmp.path().join("goodreads.csv");
    std::fs::write(
        &csv,
        "Title,Author,ISBN,My Rating,Date Read,Date Added,Bookshelves,Review\n\
         Station Eleven,Emily St. John Mandel,\"=\"\"9781447268963\"\"\",0,,2020/01/01,\"to-read, favourites\",\n",
    )
    .unwrap();
    api.import_goodreads(&csv, false, false).await.unwrap();

    let offered = match ok(api.dispatch(Request::ShelfSuggestions { preview: 3 }).await) {
        Response::ShelfSuggestions(v) => v,
        other => panic!("{other:?}"),
    };
    assert_eq!(offered[0].source, "goodreads");
    assert_eq!(offered[0].book_count, 1);
    assert_eq!(offered[0].preview[0].id, id);
    let mut tags: Vec<_> = offered.into_iter().map(|s| s.tag).collect();
    tags.sort();
    assert_eq!(tags, ["favourites", "to-read"]);

    let kept = list_of(ok(api
        .dispatch(Request::KeepShelf {
            source: "goodreads".into(),
            tag: "to-read".into(),
        })
        .await));
    assert_eq!((kept.source.as_str(), kept.book_count), ("goodreads", 1));
    ok(api
        .dispatch(Request::DismissShelf {
            source: "goodreads".into(),
            tag: "favourites".into(),
        })
        .await);
    match ok(api.dispatch(Request::ShelfSuggestions { preview: 0 }).await) {
        Response::ShelfSuggestions(v) => {
            assert!(v.is_empty(), "kept and dismissed are not offered")
        }
        other => panic!("{other:?}"),
    }
    let missing = api
        .dispatch(Request::KeepShelf {
            source: "goodreads".into(),
            tag: "no-such-shelf".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn a_copy_is_added_ended_resumed_and_deleted_by_request() {
    let (api, _tmp) = api().await;
    let book_id = save(&api, "Borrowed Book", "9780000000033").await;
    match ok(api.dispatch(Request::BookStanding { book_id }).await) {
        Response::BookStanding(s) => assert!(!s.in_library && !s.touched),
        other => panic!("{other:?}"),
    }

    let copy = copy_of(ok(api
        .dispatch(Request::AddCopy {
            book_id,
            form: None,
            tenure: TenureDto::Borrowed,
            have_now: Some(true),
            from_whom: Some("the library".into()),
            app: None,
            location: None,
            started_at: Some(1_700_000_000),
            due_at: Some(1_702_000_000),
            note: None,
        })
        .await));
    assert_eq!(copy.tenure, TenureDto::Borrowed);
    assert_eq!(copy.form, None, "an unrecorded form is none");
    assert_eq!(copy.learned_from, bookmole_api::LearnedFromDto::User);
    assert!(copy.places.is_empty());
    match ok(api.dispatch(Request::BookStanding { book_id }).await) {
        Response::BookStanding(s) => assert!(s.in_library),
        other => panic!("{other:?}"),
    }

    let ended = copy_of(ok(api
        .dispatch(Request::EndCopy {
            copy_id: copy.id,
            ended_at: Some(1_701_000_000),
        })
        .await));
    assert_eq!(
        (ended.have_now, ended.ended_at),
        (Some(false), Some(1_701_000_000))
    );
    match ok(api.dispatch(Request::BookStanding { book_id }).await) {
        Response::BookStanding(s) => {
            assert!(!s.in_library, "untouched and not had: not in the library")
        }
        other => panic!("{other:?}"),
    }

    let again = copy_of(ok(api
        .dispatch(Request::ResumeCopy { copy_id: copy.id })
        .await));
    assert_eq!((again.have_now, again.ended_at), (Some(true), None));

    let edited = copy_of(ok(api
        .dispatch(Request::UpdateCopy {
            copy_id: copy.id,
            form: Some(CopyFormDto::App),
            tenure: TenureDto::Borrowed,
            from_whom: None,
            app: Some("Libby".into()),
            location: None,
            started_at: None,
            due_at: None,
            ended_at: None,
            lent_to: None,
            unreadable: false,
            note: Some("renewed".into()),
        })
        .await));
    assert_eq!(edited.app.as_deref(), Some("Libby"));
    assert_eq!(
        edited.from_whom, None,
        "an update writes the fields as given"
    );
    assert_eq!(
        edited.have_now,
        Some(true),
        "only End/Resume write have_now"
    );

    match ok(api.dispatch(Request::CopiesForBook { book_id }).await) {
        Response::Copies(v) => assert_eq!(v.len(), 1),
        other => panic!("{other:?}"),
    }
    ok(api.dispatch(Request::DeleteCopy { copy_id: copy.id }).await);
    match ok(api.dispatch(Request::CopiesForBook { book_id }).await) {
        Response::Copies(v) => assert!(v.is_empty()),
        other => panic!("{other:?}"),
    }
    for request in [
        Request::DeleteCopy { copy_id: copy.id },
        Request::EndCopy {
            copy_id: copy.id,
            ended_at: None,
        },
        Request::ResumeCopy { copy_id: copy.id },
    ] {
        assert_eq!(
            api.dispatch(request).await.unwrap_err().code,
            ErrorCode::NotFound
        );
    }
    let orphan = api
        .dispatch(Request::AddCopy {
            book_id: 999,
            form: None,
            tenure: TenureDto::Own,
            have_now: None,
            from_whom: None,
            app: None,
            location: None,
            started_at: None,
            due_at: None,
            note: None,
        })
        .await
        .unwrap_err();
    assert_eq!(orphan.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn the_library_hides_a_copyless_book_and_filters_by_copy() {
    let (api, _tmp) = api().await;
    let owned = save(&api, "Owned", "9780000000040").await;
    let loan = save(&api, "Loan", "9780000000057").await;
    let gone = save(&api, "Gone", "9780000000064").await;
    let none = save(&api, "No Copy", "9780000000071").await;

    let count = |api: Api| async move {
        match ok(api.dispatch(Request::CountBooks { filter: None }).await) {
            Response::Count(n) => n,
            other => panic!("{other:?}"),
        }
    };
    let table_ids = |api: Api| async move {
        let mut ids: Vec<i64> = match ok(api.dispatch(Request::LibraryTable).await) {
            Response::LibraryTable(t) => t.rows.iter().map(|r| r.book.id).collect(),
            other => panic!("{other:?}"),
        };
        ids.sort();
        ids
    };
    assert_eq!(count(api.clone()).await, 0, "no copies: an empty library");
    assert!(table_ids(api.clone()).await.is_empty());

    let add = |book_id, tenure, have_now| {
        let api = api.clone();
        async move {
            copy_of(ok(api
                .dispatch(Request::AddCopy {
                    book_id,
                    form: None,
                    tenure,
                    have_now,
                    from_whom: None,
                    app: None,
                    location: None,
                    started_at: None,
                    due_at: None,
                    note: None,
                })
                .await))
        }
    };
    add(owned, TenureDto::Own, None).await;
    add(loan, TenureDto::Borrowed, Some(true)).await;
    let gone_copy = add(gone, TenureDto::Unrecorded, None).await;

    assert_eq!(count(api.clone()).await, 3);
    match ok(api
        .dispatch(Request::ListBooks {
            limit: -1,
            sort: Default::default(),
            offset: 0,
            filter: None,
        })
        .await)
    {
        Response::Books(b) => {
            let mut ids: Vec<i64> = b.iter().filter_map(|b| b.id).collect();
            ids.sort();
            assert_eq!(ids, [owned, loan, gone]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(table_ids(api.clone()).await, [owned, loan, gone]);
    assert!(api.get_book(none).await.unwrap().is_some());

    ok(api
        .dispatch(Request::EndCopy {
            copy_id: gone_copy.id,
            ended_at: None,
        })
        .await);
    assert_eq!(table_ids(api.clone()).await, [owned, loan]);

    match ok(api.dispatch(Request::LibraryCopies).await) {
        Response::LibraryCopies(l) => {
            assert_eq!(l.counts.have_now, 2);
            assert_eq!(l.counts.borrowed, 1);
            assert_eq!(
                l.counts.had, 0,
                "an ended, untouched copy is not in the library"
            );
            assert_eq!(l.counts.copy_not_recorded, 0);
            let row = l.rows.iter().find(|r| r.book_id == loan).unwrap();
            assert!(row.have_now && row.borrowed && !row.had);
            assert_eq!(row.tenure, TenureDto::Borrowed);
        }
        other => panic!("{other:?}"),
    }

    api.update_progress(gone, Some(10), None).await.unwrap();
    match ok(api.dispatch(Request::LibraryCopies).await) {
        Response::LibraryCopies(l) => {
            assert_eq!((l.counts.had, l.counts.copy_not_recorded), (1, 1));
            let row = l.rows.iter().find(|r| r.book_id == gone).unwrap();
            assert!(row.had && !row.have_now && row.copy_not_recorded);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(table_ids(api.clone()).await, [owned, loan, gone]);

    add(none, TenureDto::Own, None).await;
    assert_eq!(count(api).await, 4);
}

#[tokio::test]
async fn a_candidate_goes_on_a_list_without_being_owned() {
    let (api, _tmp) = api().await;
    let list = list_of(ok(api
        .dispatch(Request::CreateList {
            name: "Next".into(),
            description: None,
        })
        .await));
    let candidate = BookDto {
        title: Some("The Dispossessed".into()),
        authors: vec!["Ursula K. Le Guin".into()],
        isbn_13: Some("9780061054884".into()),
        ..Default::default()
    };
    let request = || Request::AddCandidateToList {
        list_id: list.id,
        book: candidate.clone(),
        note: Some("recommended".into()),
    };
    let entry = match ok(api.dispatch(request()).await) {
        Response::ListEntry(e) => e,
        other => panic!("{other:?}"),
    };
    assert!(!entry.in_library);
    let book_id = entry.book.id;
    assert!(api.copies_for_book(book_id).await.unwrap().is_empty());
    assert!(api.get_book(book_id).await.unwrap().is_some());

    match ok(api.dispatch(request()).await) {
        Response::ListEntry(again) => {
            assert_eq!(again.book.id, book_id);
            assert_eq!(again.position, entry.position);
        }
        other => panic!("{other:?}"),
    }
    let entries = api
        .list_entries(list.id, EntryShowDto::All, EntrySortDto::Yours)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);

    let owned = seed(&api).await;
    let again = api
        .add_candidate_to_list(
            list.id,
            BookDto {
                title: Some("Station Eleven".into()),
                isbn_13: Some("9781447268963".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(again.book.id, owned);
    assert!(again.in_library);

    let err = api
        .add_candidate_to_list(9999, candidate, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn keep_a_copy_here_is_refused_where_there_is_no_file() {
    let (api, _tmp) = api().await;
    let book = seed(&api).await;
    let copy = api.copies_for_book(book).await.unwrap().remove(0);
    assert!(!copy.can_keep_here);
    assert_eq!(copy.format, None);
    assert!(copy.places.is_empty());

    let err = api
        .dispatch(Request::KeepCopyHere { copy_id: copy.id })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    let err = api
        .dispatch(Request::KeepCopyHere { copy_id: 9999 })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn highlight_origins_round_trip() {
    let (api, _tmp) = api().await;
    let book = api
        .save_book(BookDto {
            title: Some("Middlemarch".into()),
            authors: vec!["George Eliot".into()],
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    have(&api, book).await;
    let sdr = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/fixtures/koreader/synthetic/Middlemarch.sdr");
    ok(api
        .dispatch(Request::ImportKoreader {
            path: sdr.display().to_string(),
            dry_run: false,
        })
        .await);
    let mine = api
        .create_highlight(book, "a line I wrote down myself", None, None)
        .await
        .unwrap();

    let origins = match ok(api
        .dispatch(Request::HighlightOrigins { book_id: book })
        .await)
    {
        Response::HighlightOrigins(o) => o,
        other => panic!("{other:?}"),
    };
    let ko: Vec<_> = origins
        .iter()
        .filter(|o| o.origin == bookmole_api::HighlightOriginDto::Koreader)
        .collect();
    assert!(!ko.is_empty());
    assert!(ko.iter().all(|o| o.copy_id.is_some()
        && o.pos_kind == Some(bookmole_api::PosKindDto::KoreaderXpointer)
        && !o.on_this_computer
        && o.device_name.is_none()));
    let user = origins
        .iter()
        .find(|o| o.highlight_id == mine.id)
        .expect("the user's highlight");
    assert_eq!(user.origin, bookmole_api::HighlightOriginDto::User);
    assert_eq!(user.copy_id, None);

    let json = serde_json::to_string(user).unwrap();
    assert!(json.contains(r#""origin":"user""#), "{json}");

    match ok(api.dispatch(Request::SamePassages { book_id: book }).await) {
        Response::SamePassages(groups) => assert!(groups.is_empty()),
        other => panic!("{other:?}"),
    }
}

fn kindle_fixture(rel: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/fixtures/kindle")
        .join(rel);
    assert!(p.is_file(), "committed fixture {} is missing", p.display());
    p.display().to_string()
}

#[tokio::test]
async fn kindle_clippings_import_and_attach_by_request() {
    let (api, tmp) = api().await;
    let book = |title: &str, author: &str| {
        let api = api.clone();
        let (title, author) = (title.to_string(), author.to_string());
        async move {
            api.save_book(BookDto {
                title: Some(title),
                authors: vec![author],
                ..Default::default()
            })
            .await
            .unwrap()
            .id
            .unwrap()
        }
    };
    let kindred = book("Kindred", "Octavia E. Butler").await;
    book(
        "Gödel, Escher, Bach: an Eternal Golden Braid (20th Anniversary Edition)",
        "Douglas R. Hofstadter",
    )
    .await;
    book("Middlemarch", "George Eliot").await;
    let path = kindle_fixture("clippings/crlf/My Clippings.txt");
    let unowned = "An Unowned Book (Nobody, A.)";

    let report = |r: Response| match r {
        Response::KindleClippingsReport(r) => r,
        other => panic!("{other:?}"),
    };
    let dry = report(ok(api
        .dispatch(Request::ImportKindleClippings {
            path: path.clone(),
            dry_run: true,
        })
        .await));
    assert!(dry.dry_run);
    assert_eq!(dry.path, path);
    let r = report(ok(api
        .dispatch(Request::ImportKindleClippings {
            path: path.clone(),
            dry_run: false,
        })
        .await));
    assert_eq!(r.books.len(), 3);
    assert_eq!(dry.summary.inserted, r.summary.inserted);
    assert!(r.books.iter().any(|b| b.book_id == kindred));
    assert_eq!(r.summary.removed, 0);
    assert_eq!(r.unmatched.len(), 1);
    let u = &r.unmatched[0];
    assert_eq!(u.title_line, unowned);
    assert_eq!(u.entries, 2);
    assert!(
        r.summary
            .diagnostics
            .iter()
            .any(|d| d.display.contains("An Unowned Book"))
    );
    let json = serde_json::to_value(&r).unwrap();
    assert!(json["unmatched"][0]["candidates"].is_array(), "{json}");

    let attached = report(ok(api
        .dispatch(Request::AttachKindleTitle {
            path: path.clone(),
            title_line: u.title_line.clone(),
            book_id: kindred,
        })
        .await));
    assert!(!attached.dry_run);
    assert_eq!(attached.books.len(), 1);
    assert_eq!(attached.summary.inserted, 2);
    assert!(attached.unmatched.is_empty());

    let err = api
        .dispatch(Request::ImportKindleClippings {
            path: tmp.path().join("gone.txt").display().to_string(),
            dry_run: true,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    let err = api
        .dispatch(Request::ImportKindleClippings {
            path: kindle_fixture("notebook/remains.html"),
            dry_run: true,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    let err = api
        .dispatch(Request::AttachKindleTitle {
            path: tmp.path().display().to_string(),
            title_line: unowned.into(),
            book_id: kindred,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    let err = api
        .dispatch(Request::AttachKindleTitle {
            path,
            title_line: unowned.into(),
            book_id: 9_999,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);

    // Parse only: dispatching would read whatever reader is plugged into this machine.
    let parsed: Request = serde_json::from_str(r#"{"method":"find_kindle_clippings"}"#).unwrap();
    assert!(matches!(parsed, Request::FindKindleClippings));
    let parsed: Request =
        serde_json::from_str(r#"{"method":"import_mounted_kindle_clippings","params":{}}"#)
            .unwrap();
    assert!(matches!(
        parsed,
        Request::ImportMountedKindleClippings { dry_run: false }
    ));
}

#[tokio::test]
async fn a_kindle_notebook_imports_by_request() {
    let (api, _tmp) = api().await;
    let remains = api
        .save_book(BookDto {
            title: Some("The Remains of the Day".into()),
            authors: vec!["Kazuo Ishiguro".into()],
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    let report = |r: Response| match r {
        Response::KindleNotebookReport(r) => r,
        other => panic!("{other:?}"),
    };
    let r = report(ok(api
        .dispatch(Request::ImportKindleNotebook {
            path: kindle_fixture("notebook/remains.html"),
            book_id: None,
            dry_run: false,
        })
        .await));
    assert_eq!(r.book_id, Some(remains));
    assert!(r.summary.inserted > 0);
    assert_eq!(r.summary.inserted, r.entries);

    let unowned = kindle_fixture("notebook/unowned.html");
    let r = report(ok(api
        .dispatch(Request::ImportKindleNotebook {
            path: unowned.clone(),
            book_id: None,
            dry_run: false,
        })
        .await));
    assert_eq!(r.book_id, None);
    assert_eq!(r.title.as_deref(), Some("A Book Nobody Owns"));
    assert_eq!(r.summary.inserted, 0);
    let r = report(ok(api
        .dispatch(Request::ImportKindleNotebook {
            path: unowned.clone(),
            book_id: Some(remains),
            dry_run: false,
        })
        .await));
    assert_eq!(r.book_id, Some(remains));
    assert_eq!(r.summary.inserted, 1);

    let err = api
        .dispatch(Request::ImportKindleNotebook {
            path: unowned,
            book_id: Some(9_999),
            dry_run: true,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    let err = api
        .dispatch(Request::ImportKindleNotebook {
            path: kindle_fixture("clippings/crlf/My Clippings.txt"),
            book_id: None,
            dry_run: true,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

/// Points Apple Books at `root` (or nowhere) so no test reads the real container.
async fn api_with_apple_books(root: Option<PathBuf>) -> (Api, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = EngineConfig::in_memory(tmp.path());
    let engine = Engine::open(config)
        .await
        .expect("engine")
        .with_apple_books_root(root);
    (Api::new(Arc::new(engine)), tmp)
}

#[tokio::test]
async fn apple_books_with_no_data_is_a_report_and_a_link_makes_a_copy() {
    let nowhere = tempfile::tempdir().expect("tempdir");
    for root in [None, Some(nowhere.path().join("no-apple-books-here"))] {
        let (api, _tmp) = api_with_apple_books(root).await;
        let r = match ok(api
            .dispatch(Request::ImportAppleBooks { dry_run: true })
            .await)
        {
            Response::AppleBooksReport(r) => r,
            other => panic!("{other:?}"),
        };
        assert!(r.dry_run);
        assert!(r.books.is_empty());
        assert_eq!(r.annotation_db, None);
        let json = serde_json::to_value(&r).unwrap();
        assert!(
            json["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["kind"] == "apple_books_absent"),
            "{json}"
        );
    }

    let (api, _tmp) = api_with_apple_books(None).await;
    let book = seed(&api).await;
    let copy = copy_of(ok(api
        .dispatch(Request::LinkAppleBooksAsset {
            asset_id: "ASSET-1".into(),
            book_id: book,
        })
        .await));
    assert_eq!(copy.book_id, book);
    assert_eq!(copy.form, Some(CopyFormDto::App));
    assert_eq!(copy.app.as_deref(), Some("apple_books"));
    assert_eq!(copy.places.len(), 1);
    assert_eq!(copy.places[0].kind, bookmole_api::PlaceKindDto::AppleBooks);
    assert_eq!(copy.places[0].reference, "ASSET-1");
    let again = copy_of(ok(api
        .dispatch(Request::LinkAppleBooksAsset {
            asset_id: "ASSET-1".into(),
            book_id: book,
        })
        .await));
    assert_eq!(again.id, copy.id);

    let other = save(&api, "Another", "9780000000088").await;
    let err = api
        .dispatch(Request::LinkAppleBooksAsset {
            asset_id: "ASSET-1".into(),
            book_id: other,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    let err = api
        .dispatch(Request::LinkAppleBooksAsset {
            asset_id: "ASSET-2".into(),
            book_id: 9_999,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn preview_requests_add_the_file_and_refuse_what_is_not_a_pdf() {
    let (api, tmp) = api().await;
    let book = save(&api, "A Paper", "9780000000095").await;

    match ok(api
        .dispatch(Request::ImportPreviewAnnotations { book_id: book })
        .await)
    {
        Response::PreviewImport(p) => {
            assert!(p.places.is_empty());
            assert_eq!(p.totals, bookmole_api::HighlightImportDto::default());
        }
        other => panic!("{other:?}"),
    }

    let pdf = tmp.path().join("broken.pdf");
    std::fs::write(&pdf, b"this is not a pdf at all").unwrap();
    let pdf = pdf.display().to_string();
    let sum = match ok(api
        .dispatch(Request::ImportPreviewFile {
            book_id: book,
            path: pdf.clone(),
        })
        .await)
    {
        Response::HighlightImport(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!((sum.inserted, sum.removed), (0, 0));
    assert!(
        sum.diagnostics
            .iter()
            .any(|d| { serde_json::to_value(d).unwrap()["kind"] == "pdf_unreadable" }),
        "{sum:?}"
    );
    let copies = api.copies_for_book(book).await.unwrap();
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].places[0].kind, bookmole_api::PlaceKindDto::Path);

    match ok(api
        .dispatch(Request::ImportPreviewAnnotations { book_id: book })
        .await)
    {
        Response::PreviewImport(p) => {
            assert_eq!(p.places.len(), 1);
            assert_eq!(p.places[0].copy_id, copies[0].id);
            assert_eq!(p.places[0].path, pdf);
            assert_eq!(
                p.places[0].outcome,
                bookmole_api::RescanOutcomeDto::Unchanged
            );
        }
        other => panic!("{other:?}"),
    }

    let refused = |path: String, book_id: i64| {
        let api = api.clone();
        async move {
            api.dispatch(Request::ImportPreviewFile { book_id, path })
                .await
                .unwrap_err()
                .code
        }
    };
    let gone = tmp.path().join("gone.pdf").display().to_string();
    assert_eq!(refused(gone, book).await, ErrorCode::NotFound);
    let txt = tmp.path().join("notes.txt");
    std::fs::write(&txt, b"words").unwrap();
    assert_eq!(
        refused(txt.display().to_string(), book).await,
        ErrorCode::InvalidInput
    );
    assert_eq!(
        refused(tmp.path().display().to_string(), book).await,
        ErrorCode::InvalidInput
    );
    let other = save(&api, "Another Paper", "9780000000101").await;
    assert_eq!(refused(pdf, other).await, ErrorCode::InvalidInput);
    let err = api
        .dispatch(Request::ImportPreviewAnnotations { book_id: 9_999 })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn resolve_library_books_hides_a_book_only_on_a_list() {
    let (api, _tmp) = api().await;
    let owned = save(&api, "Dune", "9780441172719").await;
    have(&api, owned).await;
    let listed = save(&api, "Dune Messiah", "9780593098233").await;

    let ids = |r: Response| match r {
        Response::Books(books) => {
            let mut ids: Vec<i64> = books.iter().map(|b| b.id.unwrap()).collect();
            ids.sort();
            ids
        }
        other => panic!("{other:?}"),
    };
    let everywhere = ids(ok(api
        .dispatch(Request::ResolveBooks {
            selector: "Dune".into(),
        })
        .await));
    assert_eq!(everywhere, vec![owned, listed]);
    let mine = ids(ok(api
        .dispatch(Request::ResolveLibraryBooks {
            selector: "Dune".into(),
        })
        .await));
    assert_eq!(mine, vec![owned]);

    for selector in ["9780593098233".to_string(), listed.to_string()] {
        assert_eq!(
            ids(ok(api
                .dispatch(Request::ResolveBooks {
                    selector: selector.clone()
                })
                .await)),
            vec![listed]
        );
        assert!(
            ids(ok(api
                .dispatch(Request::ResolveLibraryBooks { selector })
                .await))
            .is_empty()
        );
    }
    assert_eq!(
        ids(ok(api
            .dispatch(Request::ResolveLibraryBooks {
                selector: owned.to_string()
            })
            .await)),
        vec![owned]
    );
    let typed: Vec<i64> = api
        .resolve_library_books("Dune")
        .await
        .unwrap()
        .iter()
        .map(|b| b.id.unwrap())
        .collect();
    assert_eq!(typed, vec![owned]);
}

#[tokio::test]
async fn a_list_says_how_many_of_its_books_are_in_the_library() {
    let (api, _tmp) = api().await;
    let a = save(&api, "Alpha", "9780000000002").await;
    let b = save(&api, "Bravo", "9780000000019").await;
    let c = save(&api, "Charlie", "9780000000026").await;
    have(&api, a).await;
    let list = list_of(ok(api
        .dispatch(Request::CreateList {
            name: "Mixed".into(),
            description: None,
        })
        .await));
    assert_eq!(list.in_library_count, 0);
    for book_id in [a, b, c] {
        api.add_to_list(list.id, book_id, None).await.unwrap();
    }
    let list = list_of(ok(api.dispatch(Request::GetList { id: list.id }).await));
    assert_eq!(
        (list.book_count, list.in_library_count, list.not_in_library),
        (3, 1, 2)
    );
    match ok(api
        .dispatch(Request::ListLists {
            sort: ListSortDto::Name,
            preview: 0,
        })
        .await)
    {
        Response::ListSummaries(lists) => {
            assert_eq!(lists.len(), 1);
            assert_eq!(
                (
                    lists[0].book_count,
                    lists[0].in_library_count,
                    lists[0].not_in_library
                ),
                (3, 1, 2)
            );
        }
        other => panic!("{other:?}"),
    }
    let json = serde_json::to_value(&list).unwrap();
    assert_eq!(json["in_library_count"], 1, "{json}");
}

#[test]
fn the_copies_place_requests_parse_from_the_smallest_payload() {
    let files: Request =
        serde_json::from_str(r#"{"method":"copy_files","params":{"book_id":3}}"#).unwrap();
    assert_eq!(files, Request::CopyFiles { book_id: 3 });
    let sources: Request =
        serde_json::from_str(r#"{"method":"copy_highlight_sources","params":{"book_id":3}}"#)
            .unwrap();
    assert_eq!(sources, Request::CopyHighlightSources { book_id: 3 });
    let found: Request = serde_json::from_str(
        r#"{"method":"relocate_copy_file","params":{"copy_id":4,"path":"/x/a.epub"}}"#,
    )
    .unwrap();
    assert_eq!(
        found,
        Request::RelocateCopyFile {
            copy_id: 4,
            path: "/x/a.epub".into()
        }
    );
}

#[tokio::test]
async fn the_copies_place_round_trips_through_dispatch() {
    let (api, tmp) = api().await;
    let book = save(&api, "A Found Book", "9780000000118").await;
    let file = tmp.path().join("A Found Book.mobi");
    std::fs::write(&file, b"0123456789").unwrap();
    ok(api
        .dispatch(Request::AddFileToBook {
            book_id: book,
            path: file.display().to_string(),
        })
        .await);
    let copy = api.copies_for_book(book).await.unwrap().remove(0);

    let files = |api: Api| async move {
        match ok(api.dispatch(Request::CopyFiles { book_id: book }).await) {
            Response::CopyFiles(f) => f,
            other => panic!("{other:?}"),
        }
    };
    let rows = files(api.clone()).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].copy_id, copy.id);
    assert_eq!(rows[0].kind, bookmole_api::PlaceKindDto::Path);
    assert_eq!(rows[0].path.as_deref(), Some(file.to_str().unwrap()));
    assert_eq!(rows[0].format.as_deref(), Some("mobi"));
    assert_eq!((rows[0].on_disk, rows[0].size), (Some(true), Some(10)));
    let json = serde_json::to_value(&rows[0]).unwrap();
    assert_eq!(json["kind"], "path");
    assert!(json.get("calibre_library").is_some_and(|v| v.is_null()));

    let moved = tmp.path().join("moved");
    std::fs::create_dir_all(&moved).unwrap();
    let new = moved.join("A Found Book.mobi");
    std::fs::rename(&file, &new).unwrap();
    let rows = files(api.clone()).await;
    assert_eq!((rows[0].on_disk, rows[0].size), (Some(false), None));

    let found = match ok(api
        .dispatch(Request::RelocateCopyFile {
            copy_id: copy.id,
            path: new.display().to_string(),
        })
        .await)
    {
        Response::Copy(c) => c,
        other => panic!("{other:?}"),
    };
    assert_eq!(found.id, copy.id);
    let refs: Vec<&str> = found.places.iter().map(|p| p.reference.as_str()).collect();
    assert!(refs.contains(&new.to_str().unwrap()), "{refs:?}");
    assert!(!refs.contains(&file.to_str().unwrap()), "{refs:?}");
    assert_eq!(found.format.as_deref(), Some("mobi"));
    let rows = files(api.clone()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path.as_deref(), Some(new.to_str().unwrap()));
    assert_eq!(rows[0].on_disk, Some(true));

    api.create_highlight(book, "a line I kept", None, None)
        .await
        .unwrap();
    let sources = match ok(api
        .dispatch(Request::CopyHighlightSources { book_id: book })
        .await)
    {
        Response::CopyHighlightSources(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].origin, bookmole_api::HighlightOriginDto::User);
    assert_eq!((sources[0].copy_id, sources[0].last_read_at), (None, None));
    let json = serde_json::to_string(&sources).unwrap();
    assert!(!json.contains("count"), "{json}");
    assert!(!json.contains("a line I kept"), "{json}");

    let code = |r: Request| {
        let api = api.clone();
        async move { api.dispatch(r).await.unwrap_err().code }
    };
    assert_eq!(
        code(Request::CopyFiles { book_id: 9_999 }).await,
        ErrorCode::NotFound
    );
    assert_eq!(
        code(Request::CopyHighlightSources { book_id: 9_999 }).await,
        ErrorCode::NotFound
    );
    assert_eq!(
        code(Request::RelocateCopyFile {
            copy_id: 9_999,
            path: new.display().to_string(),
        })
        .await,
        ErrorCode::NotFound
    );
    assert_eq!(
        code(Request::RelocateCopyFile {
            copy_id: copy.id,
            path: tmp.path().join("nowhere.mobi").display().to_string(),
        })
        .await,
        ErrorCode::NotFound
    );
    let other = save(&api, "Somebody Else", "9780000000125").await;
    have(&api, other).await;
    let theirs = api.copies_for_book(other).await.unwrap().remove(0);
    assert_eq!(
        code(Request::RelocateCopyFile {
            copy_id: theirs.id,
            path: new.display().to_string(),
        })
        .await,
        ErrorCode::InvalidInput
    );
}

/// `first_published` is not on the wire, so a client cannot send it back; an absent field must not
/// clear what the library already holds.
#[tokio::test]
async fn a_book_sent_back_by_a_client_keeps_what_the_wire_does_not_carry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let engine = Arc::new(
        Engine::open(EngineConfig::in_memory(tmp.path()))
            .await
            .expect("engine"),
    );
    let api = Api::new(engine.clone());
    let id = engine
        .save_book(&bookmole::Book {
            title: Some("Middlemarch".into()),
            authors: vec!["George Eliot".into()],
            isbn_13: Some("9780141439549".into()),
            first_published: Some(1871),
            ..Default::default()
        })
        .await
        .unwrap()
        .id
        .unwrap();
    let list = api.create_list("To reread", None).await.unwrap();

    let as_sent = || {
        let read = api.get_book(id);
        async move {
            let dto = read.await.unwrap().expect("stored");
            let wire = serde_json::to_string(&dto).unwrap();
            serde_json::from_str::<BookDto>(&wire).unwrap()
        }
    };
    let requests = [
        Request::SaveBook {
            book: as_sent().await,
        },
        Request::SetBookFields {
            book_id: id,
            fields: as_sent().await,
        },
        Request::AddCandidateToList {
            list_id: list.id,
            book: as_sent().await,
            note: None,
        },
    ];
    for request in requests {
        let name = format!("{request:?}");
        ok(api.dispatch(request).await);
        let stored = engine.get_book(id).await.unwrap().unwrap();
        assert_eq!(stored.first_published, Some(1871), "after {name}");
    }
}

fn claim(field: &str, provider: bookmole_api::ProviderIdDto) -> bookmole_api::FieldClaimDto {
    bookmole_api::FieldClaimDto {
        field: field.into(),
        provider,
    }
}

/// A frontend sends back the result's `book` and `claims` as search gave them.
#[tokio::test]
async fn a_search_result_is_saved_and_listed_with_where_its_fields_came_from() {
    use bookmole_api::ProviderIdDto::{GoogleBooks, OpenLibrary};
    let (api, _tmp) = api().await;
    let book = BookDto {
        title: Some("Middlemarch".into()),
        authors: vec!["George Eliot".into()],
        isbn_13: Some("9781900000178".into()),
        page_count: Some(880),
        language: Some("en".into()),
        ..Default::default()
    };
    let claims = vec![
        claim("title", OpenLibrary),
        claim("authors", OpenLibrary),
        claim("isbn_13", OpenLibrary),
        claim("page_count", OpenLibrary),
        claim("language", GoogleBooks),
    ];
    let saved = match ok(api
        .dispatch(Request::SaveSearchResult {
            book: book.clone(),
            claims: claims.clone(),
        })
        .await)
    {
        Response::Book(Some(b)) => b,
        other => panic!("{other:?}"),
    };
    let sources = |rows: Vec<bookmole_api::FieldSourceDto>| -> Vec<(String, String)> {
        rows.into_iter().map(|f| (f.field, f.source)).collect()
    };
    let stamped = sources(api.field_provenance(saved.id.unwrap()).await.unwrap());
    assert_eq!(
        stamped,
        vec![
            ("authors".into(), "openlibrary".into()),
            ("isbn_13".into(), "openlibrary".into()),
            ("language".into(), "googlebooks".into()),
            ("page_count".into(), "openlibrary".into()),
            ("title".into(), "openlibrary".into()),
        ]
    );

    let plain = api
        .save_book(BookDto {
            title: Some("Silas Marner".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        api.field_provenance(plain.id.unwrap())
            .await
            .unwrap()
            .is_empty(),
        "a plain save records nothing"
    );

    let list = api.create_list("Victorians", None).await.unwrap();
    let entry = match ok(api
        .dispatch(Request::AddSearchResultToList {
            list_id: list.id,
            book: BookDto {
                title: Some("Romola".into()),
                authors: vec!["George Eliot".into()],
                publisher: Some("Smith, Elder".into()),
                ..Default::default()
            },
            claims: vec![
                claim("title", GoogleBooks),
                claim("authors", GoogleBooks),
                claim("publisher", OpenLibrary),
            ],
            note: None,
        })
        .await)
    {
        Response::ListEntry(e) => e,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        sources(api.field_provenance(entry.book.id).await.unwrap()),
        vec![
            ("authors".into(), "googlebooks".into()),
            ("publisher".into(), "openlibrary".into()),
            ("title".into(), "googlebooks".into()),
        ]
    );

    let err = api
        .dispatch(Request::SaveSearchResult {
            book,
            claims: vec![claim("not_a_field", OpenLibrary)],
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
}

#[test]
fn a_search_results_claims_cross_the_wire() {
    let result = bookmole::RankedResult {
        book: bookmole::Book {
            title: Some("Middlemarch".into()),
            ..Default::default()
        },
        sources: vec![bookmole::providers::ProviderId::GoogleBooks],
        score: 1.0,
        shelf_id: None,
        claims: vec![bookmole::FieldClaim {
            field: "title".into(),
            provider: bookmole::providers::ProviderId::GoogleBooks,
        }],
    };
    let json = serde_json::to_value(bookmole_api::RankedResultDto::from(result)).unwrap();
    assert_eq!(
        json["claims"],
        serde_json::json!([{ "field": "title", "provider": "google_books" }])
    );
}

#[tokio::test]
async fn a_put_down_read_leaves_currently_reading_and_keeps_its_date_in_the_books_reads() {
    let (api, _tmp) = api().await;
    let id = seed(&api).await;
    api.update_progress(id, Some(40), None).await.unwrap();

    let down = match ok(api.dispatch(Request::PutDownReading { book_id: id }).await) {
        Response::Reading(Some(r)) => r,
        other => panic!("{other:?}"),
    };
    assert_eq!(down.status, ReadingStateDto::Abandoned);
    assert!(down.put_down_at.is_some());
    assert!(api.currently_reading(20).await.unwrap().is_empty());
    assert!(api.history_reading_now().await.unwrap().is_empty());

    let listed = api.list_readings(id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].put_down_at, down.put_down_at);
    let rows = api
        .list_reading_rows(ReadingQueryDto::default())
        .await
        .unwrap();
    assert_eq!(rows[0].reading.put_down_at, down.put_down_at);
    let json = serde_json::to_value(&listed[0]).unwrap();
    assert_eq!(json["put_down_at"], serde_json::json!(down.put_down_at));

    let code = |r: Request| {
        let api = api.clone();
        async move { api.dispatch(r).await.unwrap_err().code }
    };
    assert_eq!(
        code(Request::PutDownReading { book_id: id }).await,
        ErrorCode::InvalidInput
    );

    let up = match ok(api.dispatch(Request::PickUpReading { book_id: id }).await) {
        Response::Reading(Some(r)) => r,
        other => panic!("{other:?}"),
    };
    assert_eq!(up.status, ReadingStateDto::Reading);
    assert_eq!(up.put_down_at, None);
    assert_eq!(api.currently_reading(20).await.unwrap().len(), 1);
    assert_eq!(
        code(Request::PickUpReading { book_id: id }).await,
        ErrorCode::InvalidInput
    );
    assert_eq!(
        code(Request::PutDownReading { book_id: 9_999 }).await,
        ErrorCode::NotFound
    );
}
